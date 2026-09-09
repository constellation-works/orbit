//! Walk / git-ignore batching tests migrated for ORB-00250.

use std::fs;

use tempfile::tempdir;

use super::super::config::DocsRoot;
use super::super::walk::{
    expand_root, git_check_ignore_invocations, reset_git_check_ignore_invocations, walk_docs_roots,
};

fn init_git_repo(root: &std::path::Path) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("init")
        .arg("--quiet")
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
}

#[test]
fn walker_skips_dot_orbit_even_when_root_points_above_it() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("docs")).expect("docs dir");
    fs::write(
        root.join("docs/good.md"),
        "---\ntype: context\nsummary: Good doc\n---\nbody\n",
    )
    .expect("write good");
    fs::create_dir_all(root.join(".orbit/adrs/ADR-0001")).expect("adr dir");
    fs::write(root.join(".orbit/adrs/ADR-0001/body.md"), "# ADR\n").expect("write adr");

    let records = walk_docs_roots(root, &[DocsRoot::new(".")]).expect("walk docs");
    assert_eq!(
        records
            .iter()
            .map(|record| record.path.as_str())
            .collect::<Vec<_>>(),
        vec!["docs/good.md"]
    );
}

#[test]
fn wildcard_root_expands_workspace_relative_directories() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("apps/example/docs")).expect("docs dir");
    fs::write(
        root.join("apps/example/docs/guide.md"),
        "---\ntype: context\nsummary: Guide\n---\nbody\n",
    )
    .expect("write guide");

    let records = walk_docs_roots(root, &[DocsRoot::new("apps/*/docs/")]).expect("walk docs");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "apps/example/docs/guide.md");
}

#[test]
fn wildcard_root_rejects_paths_outside_the_workspace() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&root).expect("repo dir");
    fs::create_dir_all(outside.join("docs")).expect("outside docs dir");

    assert!(
        expand_root(&root, "../outside/*")
            .expect("expand traversal")
            .is_empty()
    );
    assert!(
        expand_root(&root, &format!("{}/*", outside.display()))
            .expect("expand absolute path")
            .is_empty()
    );
}

#[test]
fn walker_batches_git_ignore_once_per_walk() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("docs/nested")).expect("docs dir");
    fs::write(
        root.join("docs/one.md"),
        "---\ntype: context\nsummary: One doc\n---\nbody\n",
    )
    .expect("write one");
    fs::write(
        root.join("docs/nested/two.md"),
        "---\ntype: context\nsummary: Two doc\n---\nbody\n",
    )
    .expect("write two");

    reset_git_check_ignore_invocations();
    let records = walk_docs_roots(root, &[DocsRoot::new("docs/")]).expect("walk docs");

    assert_eq!(git_check_ignore_invocations(), 1);
    assert_eq!(records.len(), 2);
}

#[test]
fn ordinary_root_still_drops_gitignored_files() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    init_git_repo(root);
    fs::create_dir_all(root.join("docs")).expect("docs dir");
    fs::write(root.join(".gitignore"), "docs/ignored.md\n").expect("write gitignore");
    fs::write(
        root.join("docs/ignored.md"),
        "---\ntype: context\nsummary: Ignored doc\n---\nbody\n",
    )
    .expect("write ignored");
    fs::write(
        root.join("docs/kept.md"),
        "---\ntype: context\nsummary: Kept doc\n---\nbody\n",
    )
    .expect("write kept");

    let records = walk_docs_roots(root, &[DocsRoot::new("docs/")]).expect("walk docs");
    assert_eq!(
        records
            .iter()
            .map(|record| record.path.as_str())
            .collect::<Vec<_>>(),
        vec!["docs/kept.md"]
    );
}

#[test]
fn override_root_indexes_gitignored_files() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    init_git_repo(root);
    fs::create_dir_all(root.join("external/docs")).expect("external docs dir");
    fs::write(root.join(".gitignore"), "external/\n").expect("write gitignore");
    fs::write(
        root.join("external/docs/ignored.md"),
        "---\ntype: context\nsummary: Externally gitignored doc\n---\nbody\n",
    )
    .expect("write ignored");

    let override_root = DocsRoot {
        path: "external/docs/".to_string(),
        respect_gitignore: false,
    };
    let records = walk_docs_roots(root, &[override_root]).expect("walk docs");
    assert_eq!(
        records
            .iter()
            .map(|record| record.path.as_str())
            .collect::<Vec<_>>(),
        vec!["external/docs/ignored.md"]
    );
}

#[test]
fn override_root_still_excludes_nested_dot_orbit() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    init_git_repo(root);
    fs::create_dir_all(root.join("external/.orbit/adrs")).expect(".orbit dir");
    fs::write(root.join(".gitignore"), "external/\n").expect("write gitignore");
    fs::write(
        root.join("external/.orbit/adrs/body.md"),
        "# ADR\n---\ntype: context\nsummary: Should stay excluded\n---\nbody\n",
    )
    .expect("write nested dot-orbit doc");

    let override_root = DocsRoot {
        path: "external/".to_string(),
        respect_gitignore: false,
    };
    let records = walk_docs_roots(root, &[override_root]).expect("walk docs");
    assert!(
        records.is_empty(),
        "expected .orbit exclusion to hold under an override root: {records:?}"
    );
}
