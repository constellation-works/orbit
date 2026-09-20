use serde_json::json;

use super::super::readiness::{readiness_lines, validate_task_ids};

#[test]
fn readiness_selection_rejects_blank_and_duplicate_ids() {
    assert!(validate_task_ids(&[" ".to_string()]).is_err());
    assert!(validate_task_ids(&["ORB-1".to_string(), "ORB-1".to_string()]).is_err());
}

#[test]
fn readiness_payload_names_snapshot_limit_and_reason() {
    let text = readiness_lines(&json!({
        "capacity": { "active_leaf_runs": 5, "max_active_leaf_runs": 5, "free_slots": 0 },
        "tasks": [{ "task_id": "ORB-1", "eligible": false, "reason": "capacity_saturated" }]
    }))
    .join("\n");
    assert!(text.contains("Snapshot only"), "{text}");
    assert!(
        text.contains("ORB-1: waiting (capacity_saturated)"),
        "{text}"
    );
    assert!(
        !text.contains("Occupied slots"),
        "a payload without an occupancy block prints no phase line: {text}"
    );
}

#[test]
fn readiness_payload_separates_lock_waiting_slots_from_working_ones() {
    let text = readiness_lines(&json!({
        "capacity": {
            "active_leaf_runs": 10,
            "max_active_leaf_runs": 10,
            "free_slots": 0,
            "occupancy": {
                "phases": {
                    "implementing": 4,
                    "lock_waiting": 5,
                    "post_implementation": 1,
                    "unknown": 0,
                }
            }
        },
        "tasks": [{
            "task_id": "ORB-1",
            "eligible": false,
            "reason": "conflict_deferred",
            "blocking_task_ids": ["ORB-9", "ORB-10"],
        }]
    }))
    .join("\n");
    assert!(
        text.contains("Occupied slots: 4 implementing, 5 lock-waiting, 1 post-implementation."),
        "zero-count phases are omitted: {text}"
    );
    assert!(
        text.contains("ORB-1: waiting (conflict_deferred) blocked-by=ORB-9,ORB-10"),
        "{text}"
    );
}
