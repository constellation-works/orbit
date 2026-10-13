#![allow(clippy::expect_used)]

//! Existing `PolicyEngine` behavior exercised through the crate-root public API.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_policy::PolicyEngine;
use orbit_types::policy::{FsOperation, FsProfile, PolicyDef};
use std::collections::HashMap;

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

fn engine(
    deny_read: &[&str],
    deny_modify: &[&str],
    read: &[&str],
    modify: &[&str],
) -> PolicyEngine {
    let mut fs_profiles = HashMap::new();
    fs_profiles.insert(
        "p".to_string(),
        FsProfile {
            read: read.iter().map(|s| (*s).to_string()).collect(),
            modify: modify.iter().map(|s| (*s).to_string()).collect(),
        },
    );
    let def = PolicyDef {
        name: "matrix".to_string(),
        description: None,
        deny_read: deny_read.iter().map(|s| (*s).to_string()).collect(),
        deny_modify: deny_modify.iter().map(|s| (*s).to_string()).collect(),
        fs_profiles,
        created_at: Some(Utc::now()),
        updated_at: Some(Utc::now()),
    };
    PolicyEngine::from_def(&def).expect("valid policy def")
}

struct Case {
    /// Profile `read` rules (modify mirrors read so validation passes).
    rules: &'static [&'static str],
    path: &'static str,
    allowed: bool,
    note: &'static str,
}

fn run_read_cases(cases: &[Case]) {
    for case in cases {
        let engine = engine(&[], &[], case.rules, &[]);
        let result = engine
            .check("p", FsOperation::Read, case.path)
            .unwrap_or_else(|err| {
                panic!(
                    "{}: rules {:?} path `{}`: {err}",
                    case.note, case.rules, case.path
                )
            });
        assert_eq!(
            result.allowed, case.allowed,
            "{}: rules {:?} path `{}` matched_rule `{}`",
            case.note, case.rules, case.path, result.matched_rule
        );
    }
}

// Path validation and host exception behavior.

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

    // [ORB-14122] A wildcard negation is not a literal `<prefix>/**`, so
    // structural cover does not see it. The host exception must not grant the
    // path the profile already denies, and must not take away a later
    // profile allow outside that path.
    let wildcard_profile = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/config.toml"],
        &[("implementer", &["**"], &["**", "!**/config.toml"])],
    );
    let engine = PolicyEngine::from_def(&wildcard_profile).expect("engine");
    assert!(
        !engine
            .check("implementer", FsOperation::Modify, ".orbit/config.toml")
            .expect("check wildcard profile negation")
            .allowed,
        "a host denyModify exception must not override a profile wildcard negation"
    );
    let profile_alone = make_def(
        vec![],
        vec![],
        &[("implementer", &["**"], &["**", "!**/config.toml"])],
    );
    let engine = PolicyEngine::from_def(&profile_alone).expect("engine");
    assert!(
        !engine
            .check("implementer", FsOperation::Modify, ".orbit/config.toml")
            .expect("check profile alone")
            .allowed,
        "the profile wildcard negation denies the path with no host exception"
    );

    let exception_still_grants = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/config.toml"],
        &[("implementer", &["**"], &["**"])],
    );
    let engine = PolicyEngine::from_def(&exception_still_grants).expect("engine");
    assert!(
        engine
            .check("implementer", FsOperation::Modify, ".orbit/config.toml")
            .expect("check profile allow")
            .allowed,
        "an exception still grants a path the profile allows"
    );
    assert!(
        !engine
            .check("implementer", FsOperation::Modify, ".orbit/other.toml")
            .expect("check sibling outside the exception")
            .allowed,
        "an exact exception does not grant its siblings"
    );

    let later_outside_allow = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/config.toml"],
        &[("implementer", &["**"], &["**", "!**/config.toml", "src/**"])],
    );
    let engine = PolicyEngine::from_def(&later_outside_allow).expect("engine");
    assert!(
        engine
            .check("implementer", FsOperation::Modify, "src/config.toml")
            .expect("check later profile allow")
            .allowed,
        "replaying a profile negation inside an exception must not deny a later allow outside it"
    );
    assert!(
        !engine
            .check("implementer", FsOperation::Modify, ".orbit/config.toml")
            .expect("check exact path still denied")
            .allowed
    );

    let subtree = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/pkg/**"],
        &[("implementer", &["**"], &["**", "!**/config.toml"])],
    );
    let engine = PolicyEngine::from_def(&subtree).expect("engine");
    assert!(
        !engine
            .check("implementer", FsOperation::Modify, ".orbit/pkg/config.toml")
            .expect("check subtree wildcard carve-out")
            .allowed,
        "a wildcard negation must narrow a subtree exception"
    );
    assert!(
        engine
            .check("implementer", FsOperation::Modify, ".orbit/pkg/lib.rs")
            .expect("check subtree sibling")
            .allowed,
        "a subtree exception still grants paths the wildcard negation does not match"
    );

    let subtree_reallow = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/pkg/**"],
        &[(
            "implementer",
            &["**"],
            &["**", "!**/config.toml", ".orbit/pkg/keep/**"],
        )],
    );
    let engine = PolicyEngine::from_def(&subtree_reallow).expect("engine");
    assert!(
        engine
            .check(
                "implementer",
                FsOperation::Modify,
                ".orbit/pkg/keep/config.toml"
            )
            .expect("check nested re-allow")
            .allowed,
        "a later nested profile allow overrides the wildcard negation inside the exception"
    );
    assert!(
        !engine
            .check("implementer", FsOperation::Modify, ".orbit/pkg/config.toml")
            .expect("check carve-out outside the nested allow")
            .allowed
    );

    let wildcard_reallow = make_def(
        vec![],
        vec![".orbit/**", "!.orbit/pkg/**"],
        &[(
            "implementer",
            &["**"],
            &["**", "!**/config.toml", "**/config.toml"],
        )],
    );
    let engine = PolicyEngine::from_def(&wildcard_reallow).expect("engine");
    assert!(
        engine
            .check("implementer", FsOperation::Modify, ".orbit/pkg/config.toml")
            .expect("check later wildcard re-allow")
            .allowed,
        "a later profile allow of the same wildcard must survive inside the exception"
    );
    assert!(
        engine
            .check("implementer", FsOperation::Modify, "src/config.toml")
            .expect("check outside re-allow")
            .allowed
    );

    let single_star = make_def(
        vec![],
        vec!["workspace/**", "!workspace/docs/**"],
        &[("implementer", &["**"], &["**", "!workspace/docs/*.toml"])],
    );
    let engine = PolicyEngine::from_def(&single_star).expect("engine");
    assert!(
        !engine
            .check(
                "implementer",
                FsOperation::Modify,
                "workspace/docs/note.toml"
            )
            .expect("check single-star carve-out")
            .allowed,
        "a single-star profile negation must narrow paths it matches inside the exception"
    );
    assert!(
        engine
            .check(
                "implementer",
                FsOperation::Modify,
                "workspace/docs/keep/note.toml"
            )
            .expect("check single-star does not cross segments")
            .allowed
    );
    assert!(
        engine
            .check("implementer", FsOperation::Modify, "src/note.toml")
            .expect("check single-star stays inside the exception")
            .allowed
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

// Symlink resolution behavior at the exported `check_resolved` boundary.

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

// Glob newline and precedence behavior through `PolicyEngine::check`.

#[test]
fn newline_segment_request_matrix() {
    let newline = "secrets/a\nb";
    let sibling = "secrets/ab";
    let denied = engine(&["secrets/**"], &[], &["secrets/*"], &[]);

    let newline_decision = denied
        .check("p", FsOperation::Read, newline)
        .expect("newline name");
    let sibling_decision = denied
        .check("p", FsOperation::Read, sibling)
        .expect("ordinary sibling");
    assert!(!newline_decision.allowed, "{newline_decision:?}");
    assert!(!sibling_decision.allowed, "{sibling_decision:?}");
    assert_eq!(newline_decision.matched_rule, "secrets/**");
    assert_eq!(sibling_decision.matched_rule, newline_decision.matched_rule);

    let outside = denied
        .check("p", FsOperation::Read, "notes/a\nb")
        .expect("outside the grant");
    assert!(!outside.allowed, "{outside:?}");
    assert_eq!(outside.matched_rule, "<no matching rule>");

    run_read_cases(&[
        Case {
            rules: &["**"],
            path: "a\nb/c",
            allowed: true,
            note: "** matches a newline inside a segment",
        },
        Case {
            rules: &["**/leaf"],
            path: "a\nb/leaf",
            allowed: true,
            note: "**/ matches a newline inside a directory segment",
        },
        Case {
            rules: &["**/leaf"],
            path: "a\nbleaf",
            allowed: false,
            note: "**/ still requires the slash before the literal",
        },
        Case {
            rules: &["secrets/**"],
            path: "secrets/a\nb",
            allowed: true,
            note: "trailing /** matches a newline inside the filename",
        },
        Case {
            rules: &["secrets/**"],
            path: "secrets/a\nb/c",
            allowed: true,
            note: "trailing /** matches a newline at any depth",
        },
        Case {
            rules: &["secrets/*"],
            path: "secrets/a\nb/c",
            allowed: false,
            note: "* still stops at / when the segment contains a newline",
        },
        Case {
            rules: &["a?b"],
            path: "a\nb",
            allowed: true,
            note: "? matches one newline",
        },
        Case {
            rules: &["a?b"],
            path: "a/b",
            allowed: false,
            note: "? still does not match /",
        },
    ]);
}
#[test]
fn precedence_matrix() {
    // Within a profile, evaluation is last-match-wins over the rule list:
    // a negation listed after the allow carves out the subtree...
    let carved = engine(&[], &[], &["src/**", "!src/secret/**"], &[]);
    let result = carved
        .check("p", FsOperation::Read, "src/secret/key")
        .expect("check");
    assert!(!result.allowed, "trailing negation must win: {result:?}");
    assert_eq!(result.matched_rule, "src/secret/**");
    let still_allowed = carved
        .check("p", FsOperation::Read, "src/lib.rs")
        .expect("check");
    assert!(still_allowed.allowed, "{still_allowed:?}");

    // NOTE: ...and rule order is load-bearing: the same rules reversed let
    // the broad allow override the negation. Profile authors must list
    // carve-outs last.
    let reversed = engine(&[], &[], &["!src/secret/**", "src/**"], &[]);
    let result = reversed
        .check("p", FsOperation::Read, "src/secret/key")
        .expect("check");
    assert!(
        result.allowed,
        "last-match-wins: a later allow overrides an earlier profile negation: {result:?}"
    );

    // Global denies are appended after all profile rules, so they always win
    // regardless of profile rule order.
    let global = engine(&["src/secret/**"], &[], &["src/**"], &[]);
    let result = global
        .check("p", FsOperation::Read, "src/secret/key")
        .expect("check");
    assert!(
        !result.allowed,
        "global deny must beat profile allow: {result:?}"
    );

    // A profile listing only negations denies everything (no positive rule
    // ever matches) — fail closed.
    let negations_only = engine(&[], &[], &["!tmp/**"], &[]);
    let result = negations_only
        .check("p", FsOperation::Read, "src/lib.rs")
        .expect("check");
    assert!(!result.allowed, "{result:?}");
}
