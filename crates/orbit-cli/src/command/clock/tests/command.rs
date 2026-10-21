//! `orbit clock status` keeps its own state token for every clock status.

use super::super::command::clock_status_payload;
use crate::output::payload::{Block, View};
use orbit_core::application::routines::ClockStatus;

#[test]
fn clock_status_state_token_for_each_status_combination() {
    // (enabled, running, schedulable, issue, state): any reported issue, a
    // running paused timer and an enabled clock without a future trigger all
    // read as unhealthy.
    for (enabled, running, schedulable, issue, state) in [
        (true, Some(true), true, false, "enabled"),
        (true, Some(true), true, true, "unhealthy"),
        (true, Some(false), false, true, "unhealthy"),
        (true, None, false, false, "unhealthy"),
        (false, Some(false), false, false, "paused"),
        (false, None, false, true, "unhealthy"),
        (false, Some(true), true, true, "unhealthy"),
    ] {
        let status = ClockStatus {
            configured_cadence_seconds: 60,
            effective_cadence_seconds: schedulable.then_some(60),
            enabled,
            loaded: enabled,
            running,
            schedulable,
            health_issue: issue.then(|| "fixture issue".to_string()),
            last_tick_at: None,
            next_tick_at: None,
            platform: "test",
        };
        let case = format!("{status:?}");
        let (doc, view) = clock_status_payload(&status, None).into_view();
        assert_eq!(doc["state"], state, "{case}");
        let View::Blocks(blocks) = view else {
            panic!("clock status text: {case}");
        };
        assert!(
            matches!(blocks.as_slice(), [Block::Text(text)] if text.starts_with(&format!("clock: {state} |"))),
            "{case}"
        );
    }
}
