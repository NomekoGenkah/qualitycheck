use std::fs;
use std::path::Path;

use qualitycheck::git::{get_changed_files, ChangeSet};
use tempfile::tempdir;

fn commit_all(repo: &git2::Repository, message: &str) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.update_all(["*"].iter(), None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap();
}

fn base_of<'a>(changes: &'a ChangeSet, root: &Path, rel: &str) -> Option<&'a [u8]> {
    changes
        .files
        .iter()
        .find(|f| f.path == root.join(rel))
        .unwrap_or_else(|| panic!("{rel} not reported as changed"))
        .base_content
        .as_deref()
}

/// A repo whose working tree modifies one file, moves another, and adds a third.
fn repo_with_changes() -> (tempfile::TempDir, git2::Repository) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let repo = git2::Repository::init(root).unwrap();
    let moved_body = "pub fn moved() {\n    let a = 1;\n    let b = 2;\n    println!(\"{}\", a + b);\n}\n";
    fs::write(root.join("modified.rs"), "pub fn v1() {}\n").unwrap();
    fs::write(root.join("old_name.rs"), moved_body).unwrap();
    fs::write(root.join("untouched.rs"), "pub fn same() {}\n").unwrap();
    commit_all(&repo, "base");

    fs::write(root.join("modified.rs"), "pub fn v2() {}\n").unwrap();
    fs::remove_file(root.join("old_name.rs")).unwrap();
    fs::write(root.join("new_name.rs"), moved_body.replace("moved", "renamed")).unwrap();
    fs::write(root.join("added.rs"), "pub fn added() {}\n").unwrap();
    (tmp, repo)
}

#[test]
fn test_changed_files_carry_base_content_following_renames() {
    let (tmp, _repo) = repo_with_changes();
    let root = tmp.path();

    for base_ref in [Some("HEAD"), None] {
        let changes = get_changed_files(root, base_ref).unwrap();
        assert_eq!(changes.base, "HEAD");

        let paths: Vec<_> = changes.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            vec![root.join("added.rs"), root.join("modified.rs"), root.join("new_name.rs")],
            "base_ref {base_ref:?}"
        );

        assert_eq!(base_of(&changes, root, "modified.rs"), Some(&b"pub fn v1() {}\n"[..]));
        assert_eq!(base_of(&changes, root, "added.rs"), None);
        let moved_base = base_of(&changes, root, "new_name.rs").expect("rename should keep its base");
        assert!(String::from_utf8_lossy(moved_base).contains("pub fn moved()"));
    }
}

#[test]
fn test_changed_files_in_repository_without_commits_have_no_base() {
    let tmp = tempdir().unwrap();
    let _repo = git2::Repository::init(tmp.path()).unwrap();
    fs::write(tmp.path().join("first.rs"), "pub fn first() {}\n").unwrap();

    let changes = get_changed_files(tmp.path(), None).unwrap();
    assert_eq!(changes.base, "empty repository");
    assert_eq!(changes.files.len(), 1);
    assert_eq!(changes.files[0].base_content, None);
}
