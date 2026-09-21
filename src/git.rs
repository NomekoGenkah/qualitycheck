use std::path::{Path, PathBuf};
use git2::{Delta, Diff, DiffOptions, Repository, Status, StatusOptions};

use crate::error::GitError;

/// Discovers and returns the root working directory of the git repository if present.
pub fn find_repo_root(start_path: &Path) -> Option<PathBuf> {
    Repository::discover(start_path)
        .ok()
        .and_then(|repo| repo.workdir().map(|w| w.to_path_buf()))
}

/// Identifies files that have changed versus the base branch or working tree.
pub fn get_changed_files(
    repo_path: &Path,
    base_ref: Option<&str>,
) -> Result<Vec<PathBuf>, GitError> {
    let repo = Repository::discover(repo_path)
        .map_err(|_| GitError::NotARepository(repo_path.to_path_buf()))?;

    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::NotARepository(repo_path.to_path_buf()))?
        .to_path_buf();

    let mut changed = match base_ref {
        Some(base) => collect_diff_against_base(&repo, &workdir, base)?,
        None => collect_default_changes(&repo, &workdir)?,
    };

    changed.sort();
    changed.dedup();
    Ok(changed)
}

fn collect_diff_against_base(
    repo: &Repository,
    workdir: &Path,
    base: &str,
) -> Result<Vec<PathBuf>, GitError> {
    let object = repo
        .revparse_single(base)
        .map_err(|_| GitError::RevisionNotFound(base.to_string()))?;
    let base_tree = object.peel_to_tree()?;

    let mut diff_opts = DiffOptions::new();
    diff_opts.include_untracked(true);
    diff_opts.recurse_untracked_dirs(true);

    let diff = repo.diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut diff_opts))?;
    Ok(extract_valid_files_from_diff(&diff, workdir))
}

fn collect_default_changes(
    repo: &Repository,
    workdir: &Path,
) -> Result<Vec<PathBuf>, GitError> {
    let mut status_opts = StatusOptions::new();
    status_opts.include_untracked(true);
    status_opts.recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut status_opts))?;
    let is_dirty = statuses.iter().any(|s| is_file_status_modified(s.status()));

    if is_dirty {
        Ok(collect_dirty_workdir_paths(&statuses, workdir))
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

fn collect_dirty_workdir_paths(statuses: &git2::Statuses<'_>, workdir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in statuses.iter() {
        if is_file_status_modified(entry.status())
            && let Ok(path_str) = entry.path() {
                let full_path = workdir.join(path_str);
                if full_path.is_file() {
                    paths.push(full_path);
                }
            }
    }
    paths
}

fn collect_clean_upstream_changes(
    repo: &Repository,
    workdir: &Path,
) -> Result<Vec<PathBuf>, GitError> {
    let Some(commit) = resolve_clean_tree_base(repo)? else {
        return Ok(Vec::new());
    };

    let base_tree = commit.tree()?;
    let head_tree = repo.head().and_then(|h| h.peel_to_tree()).ok();

    let diff = repo.diff_tree_to_tree(Some(&base_tree), head_tree.as_ref(), None)?;
    Ok(extract_valid_files_from_diff(&diff, workdir))
}

fn extract_valid_files_from_diff(diff: &Diff<'_>, workdir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for delta in diff.deltas() {
        if delta.status() != Delta::Deleted
            && let Some(new_file) = delta.new_file().path() {
                let full_path = workdir.join(new_file);
                if full_path.is_file() {
                    files.push(full_path);
                }
            }
    }
    files
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
