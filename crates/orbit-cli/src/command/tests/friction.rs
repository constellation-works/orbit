//! The friction CLI is derived from the operation registry [ORB-10358]; these
//! tests are the proof it stayed argv- and output-compatible.
//!
//! `friction_help/*.txt` freeze the shipped `--help` contract. Drift is a
//! consumer-visible CLI surface change. If the new help is intentional,
//! regenerate with `ORBIT_UPDATE_HELP_GOLDENS=1` (or `make goldens UPDATE=1`)
//! and review the diff.

use clap::Parser;
use orbit_common::governance::friction::FrictionVerb;
use serde_json::{Value, json};

use super::super::{Cli, Commands, operation::RuntimeNeed};

/// Parse an argv and return the friction invocation it produced.
fn invocation(args: &[&str]) -> super::super::friction::FrictionInvocation {
    match Cli::parse_from(args.iter().copied()).command {
        Commands::Friction(command) => command.command,
        _ => panic!("expected top-level friction command"),
    }
}

#[test]
fn friction_help_matches_the_shipped_surface() {
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["orbit", "friction"],
            "friction_help/root.txt",
            include_str!("friction_help/root.txt"),
        ),
        (
            &["orbit", "friction", "add"],
            "friction_help/add.txt",
            include_str!("friction_help/add.txt"),
        ),
        (
            &["orbit", "friction", "list"],
            "friction_help/list.txt",
            include_str!("friction_help/list.txt"),
        ),
        (
            &["orbit", "friction", "show"],
            "friction_help/show.txt",
            include_str!("friction_help/show.txt"),
        ),
        (
            &["orbit", "friction", "stats"],
            "friction_help/stats.txt",
            include_str!("friction_help/stats.txt"),
        ),
        (
            &["orbit", "friction", "tags"],
            "friction_help/tags.txt",
            include_str!("friction_help/tags.txt"),
        ),
        (
            &["orbit", "friction", "update"],
            "friction_help/update.txt",
            include_str!("friction_help/update.txt"),
        ),
        (
            &["orbit", "friction", "resolve"],
            "friction_help/resolve.txt",
            include_str!("friction_help/resolve.txt"),
        ),
    ];

    for (args, relative, expected) in cases {
        super::assert_help_matches_golden(args, relative, expected);
    }
}

#[test]
fn cli_parses_friction_list() {
    let parsed = invocation(&["orbit", "friction", "list", "--status", "open"]);
    assert_eq!(parsed.spec.verb, FrictionVerb::List);
    assert_eq!(parsed.input, json!({ "status": "open" }));
    assert!(!parsed.json);
    assert_eq!(parsed.target_id(), None);
}

#[test]
fn cli_parses_friction_update() {
    let parsed = invocation(&[
        "orbit",
        "friction",
        "update",
        "F2026-05-001",
        "--status",
        "triaged",
        "--tag",
        "tooling,docs",
    ]);
    assert_eq!(parsed.spec.verb, FrictionVerb::Update);
    assert_eq!(
        parsed.input,
        json!({
            "id": "F2026-05-001",
            "status": "triaged",
            "tags": ["tooling", "docs"],
        })
    );
    assert_eq!(parsed.target_id(), Some("F2026-05-001"));
}

/// The wire field is `tags` while the flag is `--tag`, and repeats accumulate.
#[test]
fn repeated_tag_flags_accumulate_into_the_plural_wire_field() {
    let parsed = invocation(&[
        "orbit",
        "friction",
        "add",
        "--body",
        "It broke",
        "--model",
        "claude",
        "--tag",
        "tooling",
        "--tag",
        "docs,build",
    ]);
    assert_eq!(
        parsed.input,
        json!({
            "body": "It broke",
            "tags": ["tooling", "docs", "build"],
            "model": "claude",
        })
    );
}

/// Optional filters are dropped when blank so the handler sees "unset" rather
/// than "set to empty"; required fields pass through verbatim for the handler
/// to validate.
#[test]
fn blank_optional_values_are_dropped_and_required_values_pass_through() {
    let parsed = invocation(&[
        "orbit",
        "friction",
        "add",
        "--body",
        "  spaced  ",
        "--model",
        "codex",
        "--during-task",
        "   ",
        "--tag",
        " , ",
    ]);
    assert_eq!(
        parsed.input,
        json!({ "body": "  spaced  ", "model": "codex" })
    );
}

#[test]
fn integer_filters_parse_as_numbers_not_strings() {
    let parsed = invocation(&[
        "orbit", "friction", "list", "--limit", "5", "--offset", "10",
    ]);
    assert_eq!(parsed.input, json!({ "limit": 5, "offset": 10 }));
}

#[test]
fn parameterless_verbs_send_an_empty_object() {
    for verb in ["stats", "tags"] {
        let parsed = invocation(&["orbit", "friction", verb]);
        assert_eq!(parsed.input, json!({}));
    }
}

#[test]
fn json_flag_is_captured_for_every_verb() {
    let cases: &[&[&str]] = &[
        &["orbit", "friction", "list", "--json"],
        &["orbit", "friction", "show", "F2026-05-001", "--json"],
        &["orbit", "friction", "stats", "--json"],
        &["orbit", "friction", "tags", "--json"],
        &["orbit", "friction", "resolve", "F2026-05-001", "--json"],
    ];
    for args in cases {
        assert!(invocation(args).json, "{args:?}");
    }
    assert!(!invocation(&["orbit", "friction", "tags"]).json);
}

/// Every verb routes to its own tool; nothing in the CLI hardcodes the mapping.
#[test]
fn each_verb_routes_to_its_registry_tool_name() {
    let cases: &[(&[&str], &str)] = &[
        (
            &[
                "orbit", "friction", "add", "--body", "b", "--model", "codex",
            ],
            "orbit.friction.add",
        ),
        (&["orbit", "friction", "list"], "orbit.friction.list"),
        (&["orbit", "friction", "show", "F1"], "orbit.friction.show"),
        (&["orbit", "friction", "stats"], "orbit.friction.stats"),
        (&["orbit", "friction", "tags"], "orbit.friction.tags"),
        (
            &["orbit", "friction", "update", "F1", "--status", "triaged"],
            "orbit.friction.update",
        ),
        (
            &["orbit", "friction", "resolve", "F1"],
            "orbit.friction.resolve",
        ),
    ];
    for (args, tool_name) in cases {
        assert_eq!(invocation(args).spec.tool_name, *tool_name, "{args:?}");
    }
}

/// One expected audit projection: argv, audit subcommand, audit target id, and
/// JSON error preference.
type AuditCase = (
    &'static [&'static str],
    &'static str,
    Option<&'static str>,
    Option<bool>,
);

/// Audit metadata is derived from the registry too — `operation.rs` no longer
/// matches friction verb by verb.
#[test]
fn audit_metadata_is_derived_from_the_registry() {
    let cases: &[AuditCase] = &[
        (&["orbit", "friction", "list"], "list", None, None),
        (
            &["orbit", "friction", "list", "--json"],
            "list",
            None,
            Some(true),
        ),
        (
            &["orbit", "friction", "show", "F2026-05-001"],
            "show",
            Some("F2026-05-001"),
            None,
        ),
        (
            &["orbit", "friction", "resolve", "F2026-05-001"],
            "resolve",
            Some("F2026-05-001"),
            None,
        ),
        (&["orbit", "friction", "stats"], "stats", None, None),
    ];

    for (args, subcommand, target_id, json_preference) in cases {
        let operation = Cli::parse_from(args.iter().copied()).command.operation();
        assert_eq!(operation.runtime_need, RuntimeNeed::Required, "{args:?}");
        assert_eq!(
            operation.json_error_preference, *json_preference,
            "{args:?}"
        );
        let meta = operation.audit_meta.expect("friction commands are audited");
        assert_eq!(meta.command, "friction");
        assert_eq!(meta.subcommand.as_deref(), Some(*subcommand));
        assert_eq!(meta.target_type.as_deref(), Some("friction"));
        assert_eq!(meta.target_id.as_deref(), *target_id);
        assert_eq!(meta.role, "admin");
    }
}

#[test]
fn unknown_friction_subcommands_are_rejected() {
    let error = match Cli::try_parse_from(["orbit", "friction", "reindex"]) {
        Ok(_) => panic!("unknown subcommand is rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("reindex"), "{error}");
}

#[test]
fn required_arguments_are_still_enforced() {
    for args in [
        &["orbit", "friction", "add", "--model", "codex"][..],
        &["orbit", "friction", "add", "--body", "b"][..],
        &["orbit", "friction", "show"][..],
        &["orbit", "friction", "update"][..],
    ] {
        assert!(
            Cli::try_parse_from(args.iter().copied()).is_err(),
            "{args:?} should be rejected"
        );
    }
}

#[test]
fn list_projects_every_declared_filter() {
    let parsed = invocation(&[
        "orbit",
        "friction",
        "list",
        "--model",
        "codex",
        "--status",
        "open",
        "--tag",
        "tooling",
        "--month",
        "2026-05",
        "--q",
        "flaky",
        "--from",
        "2026-05-01T00:00:00Z",
        "--to",
        "2026-05-31T23:59:59Z",
        "--limit",
        "3",
        "--offset",
        "1",
    ]);
    let Value::Object(input) = parsed.input else {
        panic!("friction input is an object");
    };
    let mut keys: Vec<&str> = input.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "from", "limit", "model", "month", "offset", "q", "status", "tag", "to"
        ]
    );
}
