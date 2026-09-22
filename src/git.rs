use std::path::{Path, PathBuf};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Repository, Status, StatusOptions, Tree};

use crate::error::GitError;

/// A file changed relative to a base revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Current path in the working tree.
    pub path: PathBuf,
    /// The file's content at the base revision, following renames; `None` when the file was
    /// added after the base.
    pub base_content: Option<Vec<u8>>,
}

/// Files changed relative to a base revision, and a short name for that base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    pub base: String,
    pub files: Vec<ChangedFile>,
}

/// Discovers and returns the root working directory of the git repository if present.
pub fn find_repo_root(start_path: &Path) -> Option<PathBuf> {
    Repository::discover(start_path)
        .ok()
        .and_then(|repo| repo.workdir().map(|w| w.to_path_buf()))
}

/// Identifies files that have changed versus the base branch or working tree, with their
/// content at that base.
pub fn get_changed_files(
    repo_path: &Path,
    base_ref: Option<&str>,
) -> Result<ChangeSet, GitError> {
    let repo = Repository::discover(repo_path)
        .map_err(|_| GitError::NotARepository(repo_path.to_path_buf()))?;

    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::NotARepository(repo_path.to_path_buf()))?
        .to_path_buf();

    let mut changes = match base_ref {
        Some(base) => ChangeSet {
            base: base.to_string(),
            files: collect_diff_against_base(&repo, &workdir, base)?,
        },
        None => collect_default_changes(&repo, &workdir)?,
    };

    changes.files.sort_by(|a, b| a.path.cmp(&b.path));
    changes.files.dedup_by(|a, b| a.path == b.path);
    Ok(changes)
}

fn collect_diff_against_base(
    repo: &Repository,
    workdir: &Path,
    base: &str,
) -> Result<Vec<ChangedFile>, GitError> {
    let object = repo
        .revparse_single(base)
        .map_err(|_| GitError::RevisionNotFound(base.to_string()))?;
    let base_tree = object.peel_to_tree()?;

    collect_workdir_changes(repo, workdir, Some(&base_tree))
}

fn collect_default_changes(repo: &Repository, workdir: &Path) -> Result<ChangeSet, GitError> {
    let mut status_opts = StatusOptions::new();
    status_opts.include_untracked(true);
    status_opts.recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut status_opts))?;
    let is_dirty = statuses.iter().any(|s| is_file_status_modified(s.status()));

    if is_dirty {
        // Uncommitted changes (staged, unstaged, untracked) versus HEAD; no HEAD yet means
        // every file is new.
        let head_tree = repo.head().and_then(|h| h.peel_to_tree()).ok();
        Ok(ChangeSet {
            base: if head_tree.is_some() { "HEAD" } else { "empty repository" }.to_string(),
            files: collect_workdir_changes(repo, workdir, head_tree.as_ref())?,
        })
    } else {
        collect_clean_upstream_changes(repo, workdir)
    }
}

fn is_file_status_modified(status: Status) -> bool {
    status.is_wt_new()
        || status.is_wt_modified()
        || status.is_index_new()
        || status.is_index_modified()
        || status.is_index_renamed()
        || status.is_wt_renamed()
}

/// Changes from `base_tree` to the working tree (index included, untracked files as additions).
fn collect_workdir_changes(
    repo: &Repository,
    workdir: &Path,
    base_tree: Option<&Tree<'_>>,
) -> Result<Vec<ChangedFile>, GitError> {
    let mut diff_opts = DiffOptions::new();
    diff_opts.include_untracked(true);
    diff_opts.recurse_untracked_dirs(true);

    let mut diff = repo.diff_tree_to_workdir_with_index(base_tree, Some(&mut diff_opts))?;
    extract_changed_files(repo, &mut diff, workdir)
}

fn collect_clean_upstream_changes(
    repo: &Repository,
    workdir: &Path,
) -> Result<ChangeSet, GitError> {
    let Some(commit) = resolve_clean_tree_base(repo)? else {
        return Ok(ChangeSet {
            base: "HEAD".to_string(),
            files: Vec::new(),
        });
    };

    let base_tree = commit.tree()?;
    let head_tree = repo.head().and_then(|h| h.peel_to_tree()).ok();

    let mut diff = repo.diff_tree_to_tree(Some(&base_tree), head_tree.as_ref(), None)?;
    Ok(ChangeSet {
        base: commit.id().to_string()[..7].to_string(),
        files: extract_changed_files(repo, &mut diff, workdir)?,
    })
}

fn extract_changed_files(
    repo: &Repository,
    diff: &mut Diff<'_>,
    workdir: &Path,
) -> Result<Vec<ChangedFile>, GitError> {
    // Pair deletions with additions of similar content, so a moved file keeps its base.
    let mut find_opts = DiffFindOptions::new();
    find_opts.renames(true).for_untracked(true);
    diff.find_similar(Some(&mut find_opts))?;

    let mut files = Vec::new();
    for delta in diff.deltas() {
        if delta.status() == Delta::Deleted {
            continue;
        }
        let Some(new_file) = delta.new_file().path() else {
            continue;
        };
        let full_path = workdir.join(new_file);
        if !full_path.is_file() {
            continue;
        }

        let base_blob = delta.old_file().id();
        let base_content = if matches!(delta.status(), Delta::Added | Delta::Untracked)
            || base_blob.is_zero()
        {
            None
        } else {
            repo.find_blob(base_blob).ok().map(|blob| blob.content().to_vec())
        };

        files.push(ChangedFile {
            path: full_path,
            base_content,
        });
    }
    Ok(files)
}

fn resolve_clean_tree_base<'a>(repo: &'a Repository) -> Result<Option<git2::Commit<'a>>, GitError> {
    if let Ok(head) = repo.head()
        && head.is_branch() {
            let branch = git2::Branch::wrap(head);
            if let Ok(upstream) = branch.upstream()
                && let Ok(commit) = upstream.get().peel_to_commit() {
                    return Ok(Some(commit));
                }
        }

    let candidates = [
        "origin/main",
        "origin/master",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "main",
        "master",
    ];

    for candidate in candidates {
        if let Ok(obj) = repo.revparse_single(candidate)
            && let Ok(commit) = obj.peel_to_commit() {
                return Ok(Some(commit));
            }
    }

    if let Ok(head) = repo.head()
        && let Ok(commit) = head.peel_to_commit()
            && let Ok(parent) = commit.parent(0) {
                return Ok(Some(parent));
            }

    Ok(None)
}
