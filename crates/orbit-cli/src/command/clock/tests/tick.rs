//! A partial tick's report must survive its failure exit status.

use super::super::tick::ClockTickArgs;
use crate::command::CommandOutput;
use orbit_core::application::routines::SweepOutcome;

#[test]
fn deadline_failure_returns_nonzero_with_deferred_workspaces_in_both_views() {
    let args = ClockTickArgs {
        dry_run: false,
        verbose: false,
        json: false,
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
