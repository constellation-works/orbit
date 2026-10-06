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

/// The shipped secret globs stay fail-closed, including rust-docs under the
/// `.orbit/tmp` write exception. [ORB-14337]
#[test]
fn shipped_env_denies_reject_dotenv_files_and_rust_docs_under_tmp() {
    let deny_read = ["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"];
    let deny_modify = [
        ".orbit/**",
        "!.orbit/tmp/**",
        "**/.env",
        "**/.env.*",
        "**/*.env",
        "**/*.env.*",
    ];
    let docs = ".orbit/tmp/toolchains/1.96.0-x86_64-unknown-linux-gnu/share/doc/rust/html/core/macro.env.html";
    let named = [
        ("**/.env", ".env", FsOperation::Read),
        ("**/.env", ".env", FsOperation::Modify),
        ("**/.env.*", ".env.local", FsOperation::Read),
        ("**/.env.*", ".env.local", FsOperation::Modify),
        ("**/*.env", "secrets.env", FsOperation::Read),
        ("**/*.env", "secrets.env", FsOperation::Modify),
        ("**/*.env.*", docs, FsOperation::Read),
        ("**/*.env.*", docs, FsOperation::Modify),
    ];
    for (rule, path, operation) in named {
        let def = PolicyDef {
            name: "shipped-env".into(),
            description: None,
            deny_read: if operation == FsOperation::Read {
                vec![rule.into()]
            } else {
                vec![]
            },
            deny_modify: if operation == FsOperation::Modify {
                vec![rule.into()]
            } else {
                vec![]
            },
            fs_profiles: HashMap::from([(
                "implementer".into(),
                FsProfile {
                    read: vec!["**".into()],
                    modify: vec!["**".into()],
                },
            )]),
            created_at: None,
            updated_at: None,
        };
        let decision = PolicyEngine::from_def(&def)
            .expect("policy")
            .check("implementer", operation, path)
            .expect("check");
        assert!(
            !decision.allowed,
            "ORB-14337: {rule} must deny {path} for {operation:?}: {decision:?}"
        );
        assert_eq!(
            decision.matched_rule, rule,
            "ORB-14337: {path} must be settled by {rule}"
        );
    }

    let mut profiles = HashMap::new();
    profiles.insert(
        "implementer".into(),
        FsProfile {
            read: vec!["**".into()],
            modify: vec!["**".into()],
        },
    );
    let shipped = PolicyDef {
        name: "default-shape".into(),
        description: None,
        deny_read: deny_read.into_iter().map(str::to_string).collect(),
        deny_modify: deny_modify.into_iter().map(str::to_string).collect(),
        fs_profiles: profiles,
        created_at: None,
        updated_at: None,
    };
    let engine = PolicyEngine::from_def(&shipped).expect("shipped shape");
    for (operation, path) in [
        (FsOperation::Read, ".env"),
        (FsOperation::Modify, ".env"),
        (FsOperation::Read, ".env.local"),
        (FsOperation::Modify, ".env.local"),
        (FsOperation::Read, "secrets.env"),
        (FsOperation::Modify, "secrets.env"),
        (FsOperation::Read, docs),
        (FsOperation::Modify, docs),
    ] {
        let decision = engine
            .check("implementer", operation, path)
            .expect("shipped check");
        assert!(
            !decision.allowed,
            "ORB-14337: shipped denies must still reject {path} for {operation:?} after the tmp exception: {decision:?}"
        );
    }
    let ordinary = engine
        .check("implementer", FsOperation::Modify, ".orbit/tmp/ok.txt")
        .expect("ordinary scratch");
    assert!(
        ordinary.allowed,
        "the tmp exception must still allow a non-secret scratch file: {ordinary:?}"
    );
    let docs_decision = engine
        .check("implementer", FsOperation::Modify, docs)
        .expect("docs");
    assert_eq!(
        docs_decision.matched_rule, "**/*.env.*",
        "the later env rule must beat the .orbit/tmp exception: {docs_decision:?}"
    );
}
