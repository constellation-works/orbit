#![allow(missing_docs)]

//! Boundary tests for `PolicyEngine::check`. These guard the global
//! `denyRead` / `denyModify` last-match-wins semantics, the unknown-profile
//! error path, and matched_rule observability for audit attribution.
//! See task T20260509-7.
//!
//! Consolidated from the prior nested engine/tests/ layout (check, errors, overrides)
//! into sibling tests/engine.rs per ORB-00242 / docs/design-patterns/test_layout.md.

use super::super::engine::PolicyEngine;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::policy::FsProfile;
use orbit_types::policy::{FsOperation, PolicyDef};
use std::collections::HashMap;

/// Shared test fixture builder. Constructs a minimal `PolicyDef` for
/// exercising `PolicyEngine::check` against profile rules and global denies.
fn make_def(
    deny_read: Vec<&str>,
    deny_modify: Vec<&str>,
    profiles: &[(&str, &[&str], &[&str])],
) -> PolicyDef {
    let mut fs_profiles = HashMap::new();
    for (name, read, modify) in profiles {
        fs_profiles.insert(
            (*name).to_string(),
            FsProfile {
                read: read.iter().map(|s| (*s).to_string()).collect(),
                modify: modify.iter().map(|s| (*s).to_string()).collect(),
            },
        );
    }
    PolicyDef {
        name: "test".to_string(),
        description: None,
        deny_read: deny_read.into_iter().map(String::from).collect(),
        deny_modify: deny_modify.into_iter().map(String::from).collect(),
        fs_profiles,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    }
}

// --- Error and special-case paths (from errors.rs) ---

#[test]
fn check_rejects_parent_traversal_for_read_and_modify_paths() {
    let def = make_def(vec![], vec![], &[("default", &["**"], &["**"])]);
    let engine = PolicyEngine::from_def(&def).expect("engine");

    for (operation, path) in [
        (FsOperation::Read, "../secret.txt"),
        (FsOperation::Read, "src/../secret.txt"),
        (FsOperation::Read, "..\\secret.txt"),
        (FsOperation::Read, "src\\..\\secret.txt"),
        (FsOperation::Modify, "../secret.txt"),
        (FsOperation::Modify, "src/../secret.txt"),
        (FsOperation::Modify, "..\\secret.txt"),
        (FsOperation::Modify, "src\\..\\secret.txt"),
    ] {
        let err = engine
            .check("default", operation, path)
            .expect_err("parent traversal must be rejected");

        assert!(
            matches!(err, OrbitError::InvalidInput(_)),
            "expected InvalidInput for {operation:?} `{path}`, got {err:?}"
        );
    }
}

#[test]
fn host_modify_exception_intersects_selected_profile_authority() {
    let broad = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/resources/**"],
        &[("implementer", &["**"], &["**"])],
    );
    let engine = PolicyEngine::from_def(&broad).expect("engine");
    assert!(
        engine
            .check(
                "implementer",
                FsOperation::Modify,
                ".orbit/resources/jobs/example.yaml"
            )
            .expect("check versioned resource")
            .allowed
    );
    assert!(
        !engine
            .check(
                "implementer",
                FsOperation::Modify,
                ".orbit/unknown/example.yaml"
            )
            .expect("check unknown Orbit path")
            .allowed
    );

    let narrow = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/resources/**"],
        &[("docs", &["docs/**"], &["docs/**"])],
    );
    let engine = PolicyEngine::from_def(&narrow).expect("engine");
    assert!(
        !engine
            .check(
                "docs",
                FsOperation::Modify,
                ".orbit/resources/jobs/example.yaml"
            )
            .expect("check profile intersection")
            .allowed,
        "a host exception must not create authority absent from the profile"
    );

    let profile_denied = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/resources/**"],
        &[(
            "implementer",
            &["**"],
            &["**", "!.orbit/resources/private/**"],
        )],
    );
    let engine = PolicyEngine::from_def(&profile_denied).expect("engine");
    assert!(
        !engine
            .check(
                "implementer",
                FsOperation::Modify,
                ".orbit/resources/private/secret.yaml"
            )
            .expect("check profile negative")
            .allowed,
        "profile negative rules must continue to narrow host exceptions"
    );
}

#[test]
fn workspace_policy_cannot_add_modify_exception() {
    let global = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/resources/**"],
        &[("implementer", &["**"], &["**"])],
    );
    let workspace = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/unknown/**"],
        &[("implementer", &["**"], &["**"])],
    );
    let error = PolicyDef::merged(&global, &workspace)
        .expect_err("workspace exception must not override host protection");
    assert!(
        error
            .to_string()
            .contains("outside the host policy exception surface")
    );
}

#[test]
fn modify_exception_validation_is_fail_closed() {
    for (deny_read, deny_modify, expected) in [
        (
            vec!["!.orbit/config.yaml"],
            vec![],
            "cannot be an exception",
        ),
        (
            vec![],
            vec!["!.orbit/config.yaml"],
            "strictly contained by an earlier denyModify rule",
        ),
        (
            vec![],
            vec![".orbit/**", "!.orbit/*/config.yaml"],
            "must name an exact path or `<path>/**` subtree",
        ),
        (
            vec![],
            vec![".orbit/**", "!.orbit/**"],
            "strictly contained by an earlier denyModify rule",
        ),
    ] {
        let def = make_def(deny_read, deny_modify, &[("implementer", &["**"], &["**"])]);
        let error = PolicyEngine::from_def(&def).expect_err("invalid exception must fail");
        assert!(
            error.to_string().contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }
}

// --- [ORB-00418] Symlink-safe evaluation (check_resolved) ---

#[cfg(unix)]
#[test]
fn resolved_read_through_symlink_into_denied_subtree_is_denied() {
    use std::os::unix::fs::symlink;

    let root = tempfile::TempDir::new().expect("tempdir");
    let root_path = root.path();
    std::fs::create_dir(root_path.join("allowed_dir")).expect("allowed_dir");
    std::fs::create_dir(root_path.join("denied_dir")).expect("denied_dir");
    std::fs::write(root_path.join("denied_dir/secret.txt"), b"topsecret").expect("secret");
    // A symlink inside the allowed subtree pointing into the denied subtree.
    symlink(
        root_path.join("denied_dir"),
        root_path.join("allowed_dir/link"),
    )
    .expect("symlink");

    let def = make_def(
        vec!["denied_dir/**"],
        vec!["denied_dir/**"],
        &[("default", &["./**"], &["./**"])],
    );
    let engine = PolicyEngine::from_def(&def).expect("engine");

    // Reading through the link resolves to denied_dir/secret.txt -> DENIED,
    // even though the link path itself is under an allowed subtree.
    let through_link = root_path.join("allowed_dir/link/secret.txt");
    let eval = engine
        .check_resolved(root_path, "default", FsOperation::Read, &through_link)
        .expect("check");
    assert!(
        !eval.allowed,
        "read through symlink into denied subtree must be denied: {eval:?}"
    );

    // A genuine (non-symlink) allowed path is still permitted — resolution must
    // not over-block ordinary paths.
    std::fs::write(root_path.join("allowed_dir/ok.txt"), b"ok").expect("ok file");
    let ok = engine
        .check_resolved(
            root_path,
            "default",
            FsOperation::Read,
            &root_path.join("allowed_dir/ok.txt"),
        )
        .expect("check");
    assert!(ok.allowed, "non-symlink allowed path should pass: {ok:?}");
}

#[cfg(unix)]
#[test]
fn resolved_write_to_missing_path_under_symlinked_ancestor_uses_real_location() {
    use std::os::unix::fs::symlink;

    let root = tempfile::TempDir::new().expect("tempdir");
    let root_path = root.path();
    std::fs::create_dir(root_path.join("allowed_dir")).expect("allowed_dir");
    std::fs::create_dir(root_path.join("denied_dir")).expect("denied_dir");
    symlink(
        root_path.join("denied_dir"),
        root_path.join("allowed_dir/link"),
    )
    .expect("symlink");

    let def = make_def(
        vec![],
        vec!["denied_dir/**"],
        &[("default", &["./**"], &["./**"])],
    );
    let engine = PolicyEngine::from_def(&def).expect("engine");

    // The target does not exist yet (a write/create); it must resolve against
    // the symlinked ancestor's real location for rule matching.
    let target = root_path.join("allowed_dir/link/newfile.txt");
    let eval = engine
        .check_resolved(root_path, "default", FsOperation::Modify, &target)
        .expect("check");
    assert!(
        !eval.allowed,
        "write under a symlinked ancestor must resolve to the real (denied) location: {eval:?}"
    );
    assert!(
        eval.path.contains("denied_dir/newfile.txt"),
        "resolved path should point at the real location, got `{}`",
        eval.path
    );
}

#[cfg(unix)]
#[test]
fn resolved_symlink_escaping_workspace_is_denied() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::TempDir::new().expect("outside");
    std::fs::write(outside.path().join("passwd"), b"root:x:0:0").expect("outside file");
    let root = tempfile::TempDir::new().expect("workspace");
    let root_path = root.path();
    symlink(outside.path(), root_path.join("escape")).expect("symlink");

    let def = make_def(vec![], vec![], &[("default", &["./**"], &["./**"])]);
    let engine = PolicyEngine::from_def(&def).expect("engine");

    let through = root_path.join("escape/passwd");
    let eval = engine
        .check_resolved(root_path, "default", FsOperation::Read, &through)
        .expect("check");
    assert!(
        !eval.allowed,
        "a symlink escaping the workspace root must be denied: {eval:?}"
    );
    assert_eq!(eval.matched_rule, "<outside workspace>");
}

#[cfg(unix)]
#[test]
fn resolved_dangling_symlink_into_denied_subtree_is_denied() {
    use std::os::unix::fs::symlink;

    let root = tempfile::TempDir::new().expect("tempdir");
    let root_path = root.path();
    std::fs::create_dir(root_path.join("allowed_dir")).expect("allowed_dir");
    std::fs::create_dir(root_path.join("denied_dir")).expect("denied_dir");
    // Dangling link: the target does NOT exist. `exists()` on the link path is
    // false, but an O_CREAT open through the link creates the *target*, so the
    // policy must evaluate the target location, not the link path.
    symlink(
        root_path.join("denied_dir/planted.txt"),
        root_path.join("allowed_dir/link"),
    )
    .expect("symlink");

    let def = make_def(
        vec![],
        vec!["denied_dir/**"],
        &[("default", &["./**"], &["./**"])],
    );
    let engine = PolicyEngine::from_def(&def).expect("engine");

    let eval = engine
        .check_resolved(
            root_path,
            "default",
            FsOperation::Modify,
            &root_path.join("allowed_dir/link"),
        )
        .expect("check");
    assert!(
        !eval.allowed,
        "a write through a dangling symlink must be evaluated at the link target: {eval:?}"
    );
    assert!(
        eval.path.contains("denied_dir/planted.txt"),
        "resolved path should be the dangling link's target, got `{}`",
        eval.path
    );
}

#[cfg(unix)]
#[test]
fn resolved_dangling_symlink_escaping_workspace_is_denied() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::TempDir::new().expect("outside");
    let root = tempfile::TempDir::new().expect("workspace");
    let root_path = root.path();
    // Dangling link whose (nonexistent) target lives outside the workspace.
    symlink(outside.path().join("planted.txt"), root_path.join("escape")).expect("symlink");

    let def = make_def(vec![], vec![], &[("default", &["./**"], &["./**"])]);
    let engine = PolicyEngine::from_def(&def).expect("engine");

    let eval = engine
        .check_resolved(
            root_path,
            "default",
            FsOperation::Modify,
            &root_path.join("escape"),
        )
        .expect("check");
    assert!(
        !eval.allowed,
        "a dangling symlink escaping the workspace must be denied: {eval:?}"
    );
    assert_eq!(eval.matched_rule, "<outside workspace>");
}

#[cfg(unix)]
#[test]
fn resolved_symlink_cycle_fails_closed() {
    use std::os::unix::fs::symlink;

    let root = tempfile::TempDir::new().expect("tempdir");
    let root_path = root.path();
    symlink(root_path.join("b"), root_path.join("a")).expect("symlink a");
    symlink(root_path.join("a"), root_path.join("b")).expect("symlink b");

    let def = make_def(vec![], vec![], &[("default", &["./**"], &["./**"])]);
    let engine = PolicyEngine::from_def(&def).expect("engine");

    let result = engine.check_resolved(
        root_path,
        "default",
        FsOperation::Modify,
        &root_path.join("a"),
    );
    assert!(
        result.is_err(),
        "a symlink cycle must fail closed (error), got {result:?}"
    );
}
