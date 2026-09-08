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

use std::collections::HashMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::policy::FsProfile;
use orbit_types::policy::{FsOperation, PolicyDef};

use super::super::engine::PolicyEngine;

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

// --- Glob operator depth: `**` vs `*` vs `?` ---

#[test]
fn star_depth_matrix() {
    run_read_cases(&[
        Case {
            rules: &["src/*"],
            path: "src/a.rs",
            allowed: true,
            note: "* matches one segment",
        },
        Case {
            rules: &["src/*"],
            path: "src/a/b.rs",
            allowed: false,
            note: "* must not cross a separator",
        },
        Case {
            rules: &["src/*"],
            path: "src",
            allowed: false,
            note: "src/* does not match the directory itself",
        },
        Case {
            rules: &["src/**"],
            path: "src/a/b.rs",
            allowed: true,
            note: "** crosses segments",
        },
        // NOTE: a trailing `/**` also matches the anchor directory itself —
        // `src/**` allows reading `src`, not just its descendants.
        Case {
            rules: &["src/**"],
            path: "src",
            allowed: true,
            note: "trailing /** matches anchor",
        },
        Case {
            rules: &["**/foo.rs"],
            path: "foo.rs",
            allowed: true,
            note: "leading **/ matches zero directories",
        },
        Case {
            rules: &["**/foo.rs"],
            path: "a/b/foo.rs",
            allowed: true,
            note: "leading **/ matches nested",
        },
        // NOTE: a mid-pattern `**` not followed by `/` degenerates to `.*`
        // and crosses separators: `src/**.rs` matches `src/a/b.rs`.
        Case {
            rules: &["src/**.rs"],
            path: "src/a/b.rs",
            allowed: true,
            note: "bare ** crosses separators",
        },
        Case {
            rules: &["src/?.rs"],
            path: "src/a.rs",
            allowed: true,
            note: "? matches one char",
        },
        Case {
            rules: &["src/?.rs"],
            path: "src/ab.rs",
            allowed: false,
            note: "? matches exactly one char",
        },
        Case {
            rules: &["src/a?b"],
            path: "src/a/b",
            allowed: false,
            note: "? must not match the separator",
        },
    ]);
}

// --- Hidden files and character classes ---

#[test]
fn hidden_files_and_char_class_matrix() {
    run_read_cases(&[
        // NOTE: unlike shell globs there is no dotfile special-casing —
        // `src/*` and `src/**` match hidden files. Deny rules therefore also
        // cover dotfiles, but so do allow rules.
        Case {
            rules: &["src/*"],
            path: "src/.hidden",
            allowed: true,
            note: "* matches dotfiles",
        },
        Case {
            rules: &["src/**"],
            path: "src/.git/config",
            allowed: true,
            note: "** matches hidden subtrees",
        },
        // NOTE: character classes are NOT part of the grammar; `[` and `]`
        // are literals. `[ab].rs` matches only the file literally named
        // `[ab].rs`.
        Case {
            rules: &["[ab].rs"],
            path: "a.rs",
            allowed: false,
            note: "char classes are not supported",
        },
        Case {
            rules: &["[ab].rs"],
            path: "[ab].rs",
            allowed: true,
            note: "brackets match literally",
        },
        Case {
            rules: &["a{b,c}.rs"],
            path: "ab.rs",
            allowed: false,
            note: "brace alternation is not supported",
        },
    ]);
}

// --- Case sensitivity (documented per-platform semantics, L-0062) ---

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn case_sensitivity_matrix_on_case_sensitive_platforms() {
    // NOTE: on Linux, matching follows the (case-sensitive) filesystem
    // identity: `SECRETS/key.txt` and `secrets/key.txt` are different files,
    // so a deny on `secrets/**` intentionally does not cover `SECRETS/**`.
    // A case-insensitive mount (vfat, ext4 casefold dirs) on Linux would
    // reopen this gap; that identity mismatch is out of scope here (L-0062).
    run_read_cases(&[
        Case {
            rules: &["src/**"],
            path: "SRC/lib.rs",
            allowed: false,
            note: "allow rules are case-sensitive on Linux",
        },
        Case {
            rules: &["SRC/**"],
            path: "src/lib.rs",
            allowed: false,
            note: "rule case must match path case",
        },
    ]);

    let engine = engine(&["secrets/**"], &[], &["**"], &[]);
    let result = engine
        .check("p", FsOperation::Read, "SECRETS/key.txt")
        .expect("check");
    assert!(
        result.allowed,
        "deny `secrets/**` does not cover `SECRETS/**` on case-sensitive platforms: {result:?}"
    );
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn case_sensitivity_matrix_on_case_insensitive_platforms() {
    let engine = engine(&["secrets/**"], &[], &["**"], &[]);
    let result = engine
        .check("p", FsOperation::Read, "SECRETS/key.txt")
        .expect("check");
    assert!(
        !result.allowed,
        "deny globs must cover case variants on case-insensitive platforms: {result:?}"
    );
}

// --- Unicode path identity ---

#[test]
fn unicode_paths_matrix() {
    run_read_cases(&[
        Case {
            rules: &["docs/caf\u{e9}/**"],
            path: "docs/caf\u{e9}/menu.md",
            allowed: true,
            note: "non-ascii exact match works (NFC vs NFC)",
        },
        // NOTE: no unicode normalization is applied — NFC `é` (U+00E9) and
        // NFD `e` + combining acute (U+0065 U+0301) are distinct paths. An
        // allow rule failing to match a lookalike fails closed; a deny rule
        // written in one form does NOT cover a file created in the other.
        Case {
            rules: &["docs/caf\u{e9}/**"],
            path: "docs/cafe\u{301}/menu.md",
            allowed: false,
            note: "NFD lookalike is a different path than NFC rule",
        },
        // NOTE: same for confusables: Cyrillic `а` (U+0430) is not Latin `a`.
        Case {
            rules: &["data/**"],
            path: "d\u{430}ta/x",
            allowed: false,
            note: "Cyrillic lookalike segment does not match Latin rule",
        },
    ]);

    // Deny-side documentation of the same identity gap: a deny rule in NFC
    // does not match an NFD spelling, so the broad allow wins. On-disk these
    // are genuinely different byte strings (different files) on Linux.
    let engine = engine(&["docs/caf\u{e9}/**"], &[], &["**"], &[]);
    let result = engine
        .check("p", FsOperation::Read, "docs/cafe\u{301}/menu.md")
        .expect("check");
    assert!(
        result.allowed,
        "NFC deny rule does not cover the distinct NFD path: {result:?}"
    );
}

// --- `.` / `..` segments, separators, absolute paths ---

#[test]
fn dot_segments_and_separator_matrix() {
    // [ORB-10009] Equivalent spellings of a denied file must stay denied:
    // normalization collapses `.` segments, duplicate and trailing
    // separators before matching.
    let engine = engine(&["secret/key.txt"], &[], &["**"], &[]);
    for spelling in [
        "secret/key.txt",
        "secret/./key.txt",
        "./secret/key.txt",
        "secret//key.txt",
        "secret/key.txt/",
        "secret\\key.txt",
    ] {
        let result = engine
            .check("p", FsOperation::Read, spelling)
            .expect("check");
        assert!(
            !result.allowed,
            "respelled denied path `{spelling}` must stay denied: {result:?}"
        );
    }

    // `..` anywhere is rejected outright (fail closed), never evaluated.
    let broad = engine_all();
    for path in ["../x", "a/../x", "..", "a/..", "..\\x"] {
        let err = broad
            .check("p", FsOperation::Read, path)
            .expect_err("parent traversal must be rejected");
        assert!(
            matches!(err, OrbitError::InvalidInput(_)),
            "{path}: {err:?}"
        );
    }

    // Absolute, `~`, and empty paths are rejected as invalid input.
    for path in ["/etc/passwd", "~", "~/x", "", "   "] {
        let err = broad
            .check("p", FsOperation::Read, path)
            .expect_err("path must be rejected");
        assert!(
            matches!(err, OrbitError::InvalidInput(_)),
            "{path}: {err:?}"
        );
    }
}

fn engine_all() -> PolicyEngine {
    engine(&[], &[], &["**"], &[])
}

// --- Prefix-collision traps ---

#[test]
fn prefix_collision_matrix() {
    run_read_cases(&[
        Case {
            rules: &["allowed/**"],
            path: "allowed-evil/x",
            allowed: false,
            note: "sibling dir sharing a name prefix must not match",
        },
        Case {
            rules: &["allowed/**"],
            path: "allowedevil",
            allowed: false,
            note: "bare name-prefix file must not match",
        },
        Case {
            rules: &["allowed/**"],
            path: "allowed/evil",
            allowed: true,
            note: "real child matches",
        },
        Case {
            rules: &["a/b"],
            path: "a/bc",
            allowed: false,
            note: "exact rule is anchored, not a prefix",
        },
    ]);

    // Deny-side: `secret/**` must not bleed into `secret-adjacent/**`.
    let engine = engine(&["secret/**"], &[], &["**"], &[]);
    let denied = engine
        .check("p", FsOperation::Read, "secret/x")
        .expect("check");
    assert!(!denied.allowed, "{denied:?}");
    let allowed = engine
        .check("p", FsOperation::Read, "secret-adjacent/x")
        .expect("check");
    assert!(
        allowed.allowed,
        "deny must not over-match a sibling prefix: {allowed:?}"
    );
}

// --- Empty / root patterns ---

#[test]
fn empty_and_root_matrix() {
    // Empty ruleset denies everything and attributes the deny to `[]`.
    let empty = engine(&[], &[], &[], &[]);
    let result = empty
        .check("p", FsOperation::Read, "anything")
        .expect("check");
    assert!(!result.allowed);
    assert_eq!(result.matched_rule, "[]");

    // The workspace root itself (`.`) is matched by `**` and by the literal
    // rule `.`, but not by an anchored subtree rule.
    run_read_cases(&[
        Case {
            rules: &["**"],
            path: ".",
            allowed: true,
            note: "** covers the workspace root",
        },
        Case {
            rules: &["."],
            path: ".",
            allowed: true,
            note: "literal . matches the root",
        },
        Case {
            rules: &["."],
            path: "x",
            allowed: false,
            note: "literal . matches only the root",
        },
        Case {
            rules: &["src/**"],
            path: ".",
            allowed: false,
            note: "subtree rule excludes root",
        },
        // `./**` normalizes to `**` (leading ./ stripped from rules).
        Case {
            rules: &["./**"],
            path: "a/b",
            allowed: true,
            note: "./** behaves like **",
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

// --- Read/modify asymmetry through the same matrix harness ---

#[test]
fn modify_denied_where_read_allowed_matrix() {
    let engine = engine(&[], &["src/generated/**"], &["src/**"], &["src/**"]);
    let read = engine
        .check("p", FsOperation::Read, "src/generated/out.rs")
        .expect("check");
    assert!(read.allowed, "denyModify must not affect reads: {read:?}");
    let modify = engine
        .check("p", FsOperation::Modify, "src/generated/out.rs")
        .expect("check");
    assert!(
        !modify.allowed,
        "global denyModify must deny writes: {modify:?}"
    );
}

// --- Validation rejections (rules that must never load) ---

#[test]
fn validation_rejection_matrix() {
    let build = |read: &[&str], modify: &[&str], deny_read: &[&str], deny_modify: &[&str]| {
        let mut fs_profiles = HashMap::new();
        fs_profiles.insert(
            "p".to_string(),
            FsProfile {
                read: read.iter().map(|s| (*s).to_string()).collect(),
                modify: modify.iter().map(|s| (*s).to_string()).collect(),
            },
        );
        PolicyDef {
            name: "matrix".to_string(),
            description: None,
            deny_read: deny_read.iter().map(|s| (*s).to_string()).collect(),
            deny_modify: deny_modify.iter().map(|s| (*s).to_string()).collect(),
            fs_profiles,
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    };

    struct Rejection {
        read: &'static [&'static str],
        modify: &'static [&'static str],
        deny_modify: &'static [&'static str],
        why: &'static str,
    }
    let rejected = [
        Rejection {
            read: &["../x"],
            modify: &[],
            deny_modify: &[],
            why: "parent traversal in a rule",
        },
        Rejection {
            read: &["/etc/**"],
            modify: &[],
            deny_modify: &[],
            why: "absolute rule",
        },
        Rejection {
            read: &["~/x"],
            modify: &[],
            deny_modify: &[],
            why: "home-relative rule",
        },
        Rejection {
            read: &[""],
            modify: &[],
            deny_modify: &[],
            why: "empty rule",
        },
        Rejection {
            read: &["a/**"],
            modify: &["b/**"],
            deny_modify: &[],
            why: "modify rule not covered by any read rule",
        },
        Rejection {
            read: &["a/**", "b/**"],
            modify: &["b/**"],
            deny_modify: &["b/**"],
            why: "modify rule duplicating global denyModify",
        },
    ];
    for case in rejected {
        let def = build(case.read, case.modify, &[], case.deny_modify);
        let err = PolicyEngine::from_def(&def).expect_err(case.why);
        assert!(
            matches!(err, OrbitError::InvalidInput(_)),
            "{}: {err:?}",
            case.why
        );
    }
}
