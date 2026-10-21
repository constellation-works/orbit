//! `orbit clock status` keeps its own state token for every clock status.

use super::super::command::clock_status_payload;
use crate::output::payload::{Block, View};
use orbit_core::application::routines::ClockStatus;

#[test]
fn clock_status_state_token_for_each_status_combination() {
    // (enabled, running, schedulable, issue, state): any reported issue, a
    // running paused timer and an enabled clock without a future trigger all
    // read as unhealthy.
    // Cover the full product, including synthetic combinations such as an
    // enabled, schedulable clock with an issue, which native probes don't emit.
    for (enabled, running, schedulable, issue, state) in [
        (true, Some(true), true, false, "enabled"),
        (true, Some(true), true, true, "unhealthy"),
        (true, Some(true), false, false, "unhealthy"),
        (true, Some(true), false, true, "unhealthy"),
        (true, Some(false), true, false, "enabled"),
        (true, Some(false), true, true, "unhealthy"),
        (true, Some(false), false, false, "unhealthy"),
        (true, Some(false), false, true, "unhealthy"),
        (true, None, true, false, "enabled"),
        (true, None, true, true, "unhealthy"),
        (true, None, false, false, "unhealthy"),
        (true, None, false, true, "unhealthy"),
        (false, Some(true), true, false, "unhealthy"),
        (false, Some(true), true, true, "unhealthy"),
        (false, Some(true), false, false, "unhealthy"),
        (false, Some(true), false, true, "unhealthy"),
        (false, Some(false), true, false, "paused"),
        (false, Some(false), true, true, "unhealthy"),
        (false, Some(false), false, false, "paused"),
        (false, Some(false), false, true, "unhealthy"),
        (false, None, true, false, "paused"),
        (false, None, true, true, "unhealthy"),
        (false, None, false, false, "paused"),
        (false, None, false, true, "unhealthy"),
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
