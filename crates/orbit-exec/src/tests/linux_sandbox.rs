//! Unit tests for `linux_sandbox` rule expansion — sibling layout under
//! src/tests/. The filesystem walk is platform-neutral, so these run
//! everywhere; the bwrap spawn itself is covered by the Linux-only
//! integration tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use super::{LinuxBwrapPostRunGuard, expand_rule, expand_rules, walk_paths};
use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;

fn tree() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    for dir in ["a", "a/deep", "b", "target/debug"] {
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
    for file in [
        ".env",
        "a/.env",
        "a/deep/.env.local",
        "b/settings.env",
        "b/env.txt",
        "target/debug/build.env.bak",
    ] {
        fs::write(root.join(file), b"x").expect("write file");
    }
    temp
}

fn canonical(root: &std::path::Path, rel: &str) -> PathBuf {
    root.join(rel).canonicalize().expect("canonical")
}

/// Every rule sharing a search root is matched from one walk, and the union
/// equals what the rules would have matched one at a time.
#[test]
fn rules_sharing_a_root_expand_from_one_walk_to_the_same_set() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let prefix = root.to_string_lossy().replace('\\', "/");
    let rules: Vec<String> = ["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"]
        .iter()
        .map(|glob| format!("{prefix}/{glob}"))
        .collect();

    let together = expand_rules(&rules).expect("expand together");
    let mut one_at_a_time = BTreeSet::new();
    for rule in &rules {
        one_at_a_time.extend(expand_rule(rule).expect("expand one"));
    }
    assert_eq!(together, one_at_a_time);

    let expected: BTreeSet<PathBuf> = [
        ".env",
        "a/.env",
        "a/deep/.env.local",
        "b/settings.env",
        "target/debug/build.env.bak",
    ]
    .iter()
    .map(|rel| canonical(&root, rel))
    .collect();
    assert_eq!(together, expected);
}

/// The walk lists each path exactly once: directories used to be pushed on
/// entry and again from their parent's listing.
#[test]
fn walk_lists_every_path_once() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let mut paths = Vec::new();
    walk_paths(&root, &mut paths).expect("walk");
    let unique: BTreeSet<&PathBuf> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "duplicates in {paths:?}");
    // 1 root + 4 dirs (a, a/deep, b, target, target/debug = 5) + 6 files.
    assert_eq!(paths.len(), 1 + 5 + 6);
    assert_eq!(paths[0], root);
}

fn profile(modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify,
    }
}

#[test]
fn capture_watches_absent_exact_and_subtree_denies() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let secrets = workspace.join("secrets");
    let lock = workspace.join("Cargo.lock");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", secrets.display()),
        format!("!{}", lock.display()),
    ]);

    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("absent exact/subtree denies must be guarded");
    fs::create_dir_all(&secrets).expect("create secrets");
    fs::write(secrets.join("x"), b"k").expect("write secret");
    fs::write(&lock, b"k").expect("write lock");

    let error = guard
        .verify()
        .expect_err("creating an absent deny root must fail closed");
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
}

/// macOS commonly reaches `/private/var` through the `/var` symlink. The
/// guard must match rules written through that spelling even though its walk
/// canonicalizes the search root.
#[cfg(unix)]
#[test]
fn capture_watches_absent_denies_through_a_symlinked_workspace_path() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_workspace = temp.path().join("real-workspace");
    let workspace = temp.path().join("workspace-link");
    fs::create_dir_all(&real_workspace).expect("real workspace");
    symlink(&real_workspace, &workspace).expect("workspace symlink");

    let secrets = workspace.join("secrets");
    let lock = workspace.join("Cargo.lock");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", secrets.display()),
        format!("!{}", lock.display()),
    ]);

    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("absent exact/subtree denies must be guarded");
    fs::create_dir_all(&secrets).expect("create secrets");
    fs::write(secrets.join("x"), b"k").expect("write secret");
    fs::write(&lock, b"k").expect("write lock");

    let error = guard
        .verify()
        .expect_err("creating a deny root through a symlink must fail closed");
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
}

#[test]
fn capture_skips_absent_deny_whose_nested_reallow_will_create_the_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let orbit = workspace.join(".orbit");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", orbit.display()),
        format!("{}/**", orbit.join("auto_tasks").display()),
    ]);

    assert!(
        LinuxBwrapPostRunGuard::capture(&resolved)
            .expect("capture")
            .is_none(),
        "grant preparation will create .orbit, so watching it would false-positive"
    );
}

#[test]
fn managed_aliases_replay_denies_and_pin_replaceable_parents() {
    use super::{LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT, compile_linux_bwrap_argv};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let protected = root.join("target/nested/metadata");
    fs::create_dir_all(&protected).unwrap();
    fs::write(root.join(".git"), "gitdir: target/nested/metadata").unwrap();
    let resolved = profile(vec![
        format!("{}/**", root.display()),
        format!("!{}", root.join(".git").display()),
        format!("!{}/**", protected.display()),
    ]);
    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&root), true).unwrap();
    let mounts: Vec<_> = plan
        .args
        .windows(3)
        .filter(|args| matches!(args[0].as_str(), "--bind" | "--ro-bind"))
        .collect();
    for (source, destination, mode) in [
        (
            root.join(".git"),
            PathBuf::from(LINUX_STABLE_WORKSPACE_MOUNT).join(".git"),
            "--ro-bind",
        ),
        (
            protected.clone(),
            PathBuf::from(LINUX_STABLE_BUILD_MOUNT).join("nested/metadata"),
            "--ro-bind",
        ),
        (
            root.join("target/nested"),
            root.join("target/nested"),
            "--bind",
        ),
        (
            root.join("target/nested"),
            PathBuf::from(LINUX_STABLE_BUILD_MOUNT).join("nested"),
            "--bind",
        ),
    ] {
        let final_mount = mounts
            .iter()
            .rfind(|args| args[2] == destination.display().to_string())
            .unwrap();
        assert_eq!(final_mount[0], mode);
        assert_eq!(final_mount[1], source.display().to_string());
    }
}
