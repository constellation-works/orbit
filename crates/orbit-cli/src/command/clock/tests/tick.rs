//! A partial tick's report must survive its failure exit status.

use super::super::tick::ClockTickArgs;
use crate::command::CommandOutput;
use orbit_core::application::routines::{ExternalMergeSweepReport, SweepOutcome};

#[test]
fn deadline_failure_returns_nonzero_with_deferred_workspaces_in_both_views() {
    let args = ClockTickArgs {
        dry_run: false,
        verbose: false,
    };
    let outcome = SweepOutcome {
        deadline_exceeded: true,
        skipped_workspaces: vec!["blocked".into(), "following".into()],
        ..SweepOutcome::default()
    };
    let output = args.output(&outcome);
    assert_ne!(
        output.exit_code(),
        0,
        "a deadline must fail the service start so the manager records it"
    );
    let CommandOutput::Payload(payload) = output else {
        panic!("tick report payload");
    };
    let (doc, view) = payload.into_view();
    assert_eq!(doc["deadline_exceeded"], true);
    assert_eq!(
        doc["skipped_workspaces"],
        serde_json::json!(["blocked", "following"])
    );
    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("human report blocks");
    };
    assert!(blocks.iter().any(|block| matches!(block, crate::output::payload::Block::Text(text) if text.contains("blocked") && text.contains("following"))), "deadline output must name deferred workspaces without --verbose");
}

#[test]
fn settled_tasks_list_with_pr_number_in_both_views() {
    let args = ClockTickArgs {
        dry_run: false,
        verbose: false,
    };
    let outcome = SweepOutcome {
        external_merge_settlements: vec![ExternalMergeSweepReport {
            task_id: "ORB-1".into(),
            source: "ws".into(),
            pr_number: 42,
        }],
        ..SweepOutcome::default()
    };
    let output = args.output(&outcome);
    let CommandOutput::Payload(payload) = output else {
        panic!("tick report payload");
    };
    let (doc, view) = payload.into_view();
    assert_eq!(doc["settled"][0]["task_id"], "ORB-1");
    assert_eq!(doc["settled"][0]["pr_number"], 42);
    assert_eq!(doc["external_merge_settlements"][0]["pr_number"], 42);
    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("human report blocks");
    };
    assert!(
        blocks.iter().any(|block| matches!(block, crate::output::payload::Block::Text(text) if text.contains("ORB-1") && text.contains("#42"))),
        "human output must name the settled task and PR: {blocks:?}"
    );
}
