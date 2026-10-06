use std::collections::HashMap;

use orbit_policy::PolicyEngine;
use orbit_types::policy::{FsOperation, FsProfile, PolicyDef};

fn policy(modify: &[&str], deny_modify: &[&str]) -> PolicyDef {
    PolicyDef {
        name: "test".into(),
        description: None,
        deny_read: vec![],
        deny_modify: deny_modify.iter().map(|rule| (*rule).into()).collect(),
        fs_profiles: HashMap::from([(
            "implementer".into(),
            FsProfile {
                read: vec!["**".into()],
                modify: modify.iter().map(|rule| (*rule).into()).collect(),
            },
        )]),
        created_at: None,
        updated_at: None,
    }
}

#[test]
fn workspace_modify_rules_preserve_order_across_host_exceptions() {
    // ORB-14290: a repeated workspace deny was dropped before it could narrow
    // a host exception. Repeated exceptions must also retain their position.
    for (host, workspace, path, allowed) in [
        (
            vec![".orbit/**", "!.orbit/tmp/**"],
            vec![".orbit/**"],
            ".orbit/tmp/x",
            false,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/x"],
            vec![".orbit/**"],
            ".orbit/tmp/x",
            false,
        ),
        (
            vec![".orbit/**", " !.orbit/tmp/** "],
            vec![".orbit/**"],
            ".orbit/tmp/x",
            false,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**", ".orbit/**"],
            vec![".orbit/**"],
            ".orbit/tmp/x",
            false,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**", "!.orbit/resources/**"],
            vec![".orbit/**"],
            ".orbit/resources/x",
            false,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**"],
            vec![".orbit/**", "!.orbit/tmp/**"],
            ".orbit/tmp/x",
            true,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**"],
            vec![".orbit/**", "!.orbit/tmp/**", ".orbit/**"],
            ".orbit/tmp/x",
            false,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**"],
            vec![".orbit/tmp/x", "!.orbit/tmp/**"],
            ".orbit/tmp/x",
            true,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**"],
            vec![".orbit/tmp/x", "!.orbit/tmp/**"],
            ".orbit/other/x",
            false,
        ),
        (
            vec![".orbit/**", "!.orbit/tmp/**"],
            vec![".orbit/resources/**"],
            ".orbit/tmp/x",
            true,
        ),
        (vec![".orbit/**"], vec![".orbit/**"], ".orbit/tmp/x", false),
    ] {
        let merged = PolicyDef::merged(&policy(&["**"], &host), &policy(&["**"], &workspace))
            .expect("merge valid ordered modify rules");
        let engine = PolicyEngine::from_def(&merged).expect("validated merged policy");
        let result = engine
            .check("implementer", FsOperation::Modify, path)
            .expect("check merged modify authority");
        assert_eq!(
            result.allowed, allowed,
            "ORB-14290: merge must preserve workspace deny/exception order: \
             host={host:?}, workspace={workspace:?}, path={path}, result={result:?}"
        );
    }
}

#[test]
fn modify_exception_respects_the_last_covering_profile_rule() {
    // ORB-14286: an early nested allow was replayed after a covering deny.
    for (modify, path, allowed) in [
        (vec!["a/b/x", "!a/**"], "a/b/x", false),
        (vec!["a/b/**", "!a/**"], "a/b/x", false),
        (vec!["a/b/x", "!a/b/**"], "a/b/x", false),
        (vec!["a/b/x", "!**"], "a/b/x", false),
        (vec!["!a/b/x", "**"], "a/b/x", true),
        (vec!["a/b/x", "!a/**", "a/b/keep/**"], "a/b/x", false),
        (vec!["a/b/x", "!a/**", "a/b/keep/**"], "a/b/keep/x", true),
        (vec!["a/b/x", "!a/**", "a/b/x"], "a/b/x", true),
    ] {
        let profile = policy(&modify, &[]);
        let with_exception = policy(&modify, &["a/**", "!a/b/**"]);
        for def in [&profile, &with_exception] {
            let result = def
                .check_path("implementer", FsOperation::Modify, path)
                .expect("check_path");
            assert_eq!(
                result.allowed, allowed,
                "ORB-14286: last covering rule must settle earlier nested rules: \
                 modify={modify:?}, denyModify={:?}, path={path}",
                def.deny_modify
            );
            let engine = PolicyEngine::from_def(def).expect("validated policy engine");
            assert_eq!(
                engine
                    .check("implementer", FsOperation::Modify, path)
                    .expect("engine check")
                    .allowed,
                allowed,
                "PolicyEngine must preserve check_path's authority decision"
            );
        }
    }
}

#[test]
fn modify_exceptions_never_expand_profile_authority_across_rule_orders() {
    let rules = [
        "**",
        "!**",
        "a/b/**",
        "!a/b/**",
        "!a/**",
        "a/b/x",
        "!a/b/x",
        "**/x",
        "!**/x",
        "a/b/keep/**",
    ];
    let paths = [
        "a",
        "a/b",
        "a/b/x",
        "a/b/y",
        "a/b/keep",
        "a/b/keep/x",
        "a/b/keep/y",
        "a/b/deep/x",
        "a/c/x",
        "a/c/y",
        "src/x",
        "src/y",
    ];

    // Enumerate every sequence of zero to three rules, including repeats.
    // Compile once per resolved profile through the public API, as sandbox
    // consumers do, instead of rebuilding its regexes for every probe path.
    for length in 0..=3 {
        for mut ordinal in 0..rules.len().pow(length) {
            let mut modify = Vec::new();
            for _ in 0..length {
                modify.push(rules[ordinal % rules.len()]);
                ordinal /= rules.len();
            }
            let profile = PolicyEngine::from_def(&policy(&modify, &[])).expect("profile engine");
            let profile_rules = profile
                .def()
                .effective_profile("implementer")
                .expect("profile")
                .compile(FsOperation::Modify)
                .expect("compiled profile");
            for exception in ["!a/b/**", "!a/b/x", "!a/b/keep/**", "!a/c/**"] {
                let engine = PolicyEngine::from_def(&policy(&modify, &["a/**", exception]))
                    .expect("exception engine");
                let resolved = engine
                    .def()
                    .effective_profile("implementer")
                    .expect("exception profile")
                    .compile(FsOperation::Modify)
                    .expect("compiled exception profile");
                for path in paths {
                    let before = profile_rules.evaluate(path).expect("profile check");
                    let after = resolved.evaluate(path).expect("exception check");
                    assert!(
                        !after.allowed || before.allowed,
                        "ORB-14286: a host exception cannot grant authority the profile denies: \
                         modify={modify:?}, exception={exception}, path={path}, \
                         profile={before:?}, resolved={after:?}"
                    );
                }
            }
        }
    }
}
