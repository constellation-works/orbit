#![allow(missing_docs)]

//! [ORB-10009] Table-driven allow/deny matrix for `PolicyEngine::check`.
//!
//! Asserts the policy grammar's *actual* semantics across glob edge cases,
//! case sensitivity, unicode identity, dot segments/separators, prefix
//! collisions, root/empty patterns, and allow/deny precedence. Where the
//! behavior is surprising, the case documents it with a `// NOTE:` instead of
//! silently changing enforcement semantics.
//!
//! Complements `tests/engine.rs` (boundary + [ORB-00418] symlink resolution);
//! the raw-string matching grammar itself is exercised here.

use super::super::engine::PolicyEngine;
use chrono::Utc;

use orbit_types::policy::FsProfile;
use orbit_types::policy::{FsOperation, PolicyDef};
use std::collections::HashMap;

/// Minimal engine builder mirroring `tests/engine.rs::make_def`.
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

/// Request decisions for a filename that contains a newline. A recursive deny
/// appended after a single-star grant has to win, and `**` / `**/` / trailing
/// `/**` have to allow the same name when they are the matching grant.
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

// --- Overlapping allow + deny precedence ---

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
