//! The `orbit operation` CLI is derived from the operation-mode registry
//! [ORB-11332]; these tests freeze its argv surface and input projection.
//! Help goldens regenerate with `ORBIT_UPDATE_HELP_GOLDENS=1` or
//! `make goldens UPDATE=1`.

use clap::Parser;
use orbit_common::governance::operation_mode::OperationModeVerb;
use serde_json::json;

use super::super::{Cli, Commands, operation::RuntimeNeed};

fn invocation(args: &[&str]) -> super::super::operation_mode::OperationModeInvocation {
    match Cli::parse_from(args.iter().copied()).command {
        Commands::Operation(command) => command.command,
        _ => panic!("expected top-level operation command"),
    }
}

#[test]
fn operation_help_matches_the_shipped_surface() {
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["orbit", "operation"],
            "operation_mode_help/root.txt",
            include_str!("operation_mode_help/root.txt"),
        ),
        (
            &["orbit", "operation", "explain"],
            "operation_mode_help/explain.txt",
            include_str!("operation_mode_help/explain.txt"),
        ),
        (
            &["orbit", "operation", "enable"],
            "operation_mode_help/enable.txt",
            include_str!("operation_mode_help/enable.txt"),
        ),
        (
            &["orbit", "operation", "list"],
            "operation_mode_help/list.txt",
            include_str!("operation_mode_help/list.txt"),
        ),
        (
            &["orbit", "operation", "show"],
            "operation_mode_help/show.txt",
            include_str!("operation_mode_help/show.txt"),
        ),
        (
            &["orbit", "operation", "stop"],
            "operation_mode_help/stop.txt",
            include_str!("operation_mode_help/stop.txt"),
        ),
        (
            &["orbit", "operation", "revoke"],
            "operation_mode_help/revoke.txt",
            include_str!("operation_mode_help/revoke.txt"),
        ),
    ];
    for (args, relative, expected) in cases {
        super::assert_help_matches_golden(args, relative, expected);
    }
}

#[test]
fn enable_projects_scope_window_rights_and_run_layer_into_tool_input() {
    let parsed = invocation(&[
        "orbit",
        "operation",
        "enable",
        "--task",
        "ORB-1,ORB-2",
        "--task",
        "ORB-3",
        "--for",
        "2h",
        "--right",
        "prepare,promote",
        "--preset",
        "autonomous",
        "--leaf-ceiling",
        "4",
        "--review-policy",
        "none",
        "--json",
    ]);
    assert_eq!(parsed.spec.verb, OperationModeVerb::Enable);
    assert!(parsed.json);
    assert_eq!(
        parsed.input,
        json!({
            "task_ids": ["ORB-1", "ORB-2", "ORB-3"],
            "window": "2h",
            "rights": ["prepare", "promote"],
            "preset": "autonomous",
            "leaf_ceiling": 4,
            "review_policy": "none",
        })
    );
}

#[test]
fn stop_and_revoke_carry_the_compare_and_set_revision() {
    let parsed = invocation(&[
        "orbit",
        "operation",
        "stop",
        "--id",
        "ogrant-1",
        "--if-revision",
        "3",
        "--reason",
        "window closed early",
    ]);
    assert_eq!(parsed.spec.verb, OperationModeVerb::Stop);
    assert_eq!(
        parsed.input,
        json!({ "id": "ogrant-1", "if_revision": 3, "reason": "window closed early" })
    );
    let revoke = invocation(&["orbit", "operation", "revoke"]);
    assert_eq!(revoke.spec.verb, OperationModeVerb::Revoke);
    assert_eq!(revoke.input, json!({}));
    assert_eq!(
        invocation(&["orbit", "operation", "show", "ogrant-9"]).target_id(),
        Some("ogrant-9")
    );
}

#[test]
fn operation_commands_need_a_runtime_and_carry_admin_audit_metadata() {
    let cli = Cli::parse_from(["orbit", "operation", "explain"]);
    let operation = cli.command.operation();
    assert_eq!(operation.runtime_need, RuntimeNeed::Required);
    let meta = operation
        .audit_meta
        .expect("operation commands are audited");
    assert_eq!(meta.command, "operation");
    assert_eq!(meta.subcommand.as_deref(), Some("explain"));
}

#[test]
fn hidden_pipeline_worker_uses_bounded_bootstrap_recovery() {
    let cli = Cli::parse_from(["orbit", "job", "run-pipeline-worker", "jrun-child"]);
    assert_eq!(
        cli.command.operation().runtime_need,
        RuntimeNeed::PipelineWorker
    );
}

#[test]
fn run_auto_grant_conflicts_with_blanket_completion() {
    assert!(
        Cli::try_parse_from(["orbit", "run", "auto", "--grant", "ogrant-1", "--complete"]).is_err()
    );
    assert!(
        Cli::try_parse_from(["orbit", "run", "auto", "--grant", "ogrant-1", "--stop"]).is_err()
    );
    assert!(
        Cli::try_parse_from(["orbit", "run", "auto", "--grant", "ogrant-1", "--for", "1h"]).is_ok()
    );
}
