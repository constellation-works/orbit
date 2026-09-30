use std::collections::BTreeSet;

use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::json;

use super::super::format::{LockHolders, format_waiting_line_with_holders};
use super::super::lock_holders::resolve_lock_holders;

fn locks() -> serde_json::Value {
    json!({
        "by_task": [
            { "id": "ORB-1", "context_files": ["file:src/lib.rs"] },
            { "id": "ORB-2", "context_files": ["file:docs/a.md"] },
        ],
        "by_reservation": [
            { "task_ids": ["ORB-3"], "files": ["file:src/main.rs"] },
        ],
    })
}

#[test]
fn a_waited_on_selector_names_the_task_holding_an_overlapping_lock() {
    let holders = resolve_lock_holders(
        &[
            "file:src/lib.rs".to_string(),
            "file:src/main.rs".to_string(),
        ],
        &locks(),
        &BTreeSet::new(),
    );
    assert_eq!(holders["file:src/lib.rs"], vec!["ORB-1".to_string()]);
    assert_eq!(holders["file:src/main.rs"], vec!["ORB-3".to_string()]);
}

#[test]
fn a_run_is_never_its_own_holder_and_a_free_selector_names_nobody() {
    let own = BTreeSet::from(["ORB-1".to_string()]);
    let holders = resolve_lock_holders(
        &["file:src/lib.rs".to_string(), "file:README.md".to_string()],
        &locks(),
        &own,
    );
    assert!(holders.is_empty(), "{holders:?}");
}

#[test]
fn the_waiting_line_names_holders_and_falls_back_to_the_bare_selector() {
    let mut state = PipelineState::new(
        "jrun-t".to_string(),
        "task_gate_pipeline".to_string(),
        json!({}),
    );
    state.set_waiting_reasons(
        None,
        Some(vec![
            "file:src/lib.rs".to_string(),
            "file:other.rs".to_string(),
        ]),
    );
    let holders = LockHolders::from([("file:src/lib.rs".to_string(), vec!["ORB-1".to_string()])]);

    assert_eq!(
        format_waiting_line_with_holders(JobRunState::Running, Some(&state), &holders),
        Some("Waiting on locks: file:src/lib.rs (held by ORB-1), file:other.rs".to_string())
    );
}
