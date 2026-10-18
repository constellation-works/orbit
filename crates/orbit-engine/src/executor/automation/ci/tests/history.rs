use std::collections::BTreeSet;
use std::fs;

use serde_json::json;

use super::super::history::{RetryableHistory, history_path, load, save};

#[test]
fn persisted_retryable_errors_degrade_only_after_three_distinct_sweeps() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = history_path(directory.path()).expect("history path");
    let error = json!({
        "run_id": 42,
        "job_id": 24,
        "operation": "job_log_truncated",
        "retryable": true,
    });
    let mut history = RetryableHistory::default();

    for sweep_id in ["scheduled-1", "scheduled-1", "scheduled-2"] {
        let observed = history.observe(vec![error.clone()], sweep_id);
        assert_eq!(observed.retryable.len(), 1);
        assert!(observed.persistent.is_empty());
        save(&path, &history).expect("persist history");
        history = load(&path);
    }

    let observed = history.observe(vec![error], "scheduled-3");
    assert!(observed.retryable.is_empty());
    assert_eq!(observed.persistent.len(), 1);
    assert_eq!(observed.persistent[0]["consecutive_sweeps"], json!(3));
}

#[test]
fn old_shape_history_keeps_streaks_and_keys_and_round_trips() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = history_path(directory.path()).expect("history path");
    let parent = path.parent().expect("history directory");
    orbit_common::fs::io::create_private_dir_all(parent).expect("history directory");

    // Today's writer emitted this shape. Keys are `Value`'s display of the
    // id, so a string id is quoted and a missing job is `-`. One count and
    // one landing gap are malformed and must be dropped on their own.
    let fixture = json!({
        "note": "ignored",
        "schema_version": "ignored",
        "landing_gaps": {
            "bad-gap": {"last_sweep_id": "only"},
            "branch/workflow/job": {"consecutive_sweeps": 4, "last_sweep_id": "sweep-a"}
        },
        "counts": {
            "null-entry": null,
            "broken-shape": 1,
            "8/9/": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a", "extra": true},
            "malformed": {"consecutive_sweeps": "nope"},
            "7/-/check": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"},
            "42/24/job_log_truncated": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"},
            "\"run-7\"/\"99\"/logs": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"}
        }
    });
    fs::write(&path, fixture.to_string()).expect("write old shape");

    let mut history = load(&path);
    save(&path, &history).expect("save cleaned history");
    assert_eq!(
        fs::read_to_string(&path).expect("read cleaned history"),
        json!({
            "schema_version": 2,
            "counts": {
                "\"run-7\"/\"99\"/logs": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"},
                "42/24/job_log_truncated": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"},
                "7/-/check": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"},
                "8/9/": {"consecutive_sweeps": 2, "last_sweep_id": "sweep-a"}
            },
            "landing_gaps": {
                "branch/workflow/job": {"consecutive_sweeps": 4, "last_sweep_id": "sweep-a"}
            }
        })
        .to_string()
    );

    let observed = history.observe(
        vec![
            json!({"run_id": "run-7", "job_id": "99", "operation": "logs"}),
            json!({"run_id": 42, "job_id": 24, "operation": "job_log_truncated"}),
            json!({"run_id": 7, "operation": "check"}),
            json!({"run_id": 8, "job_id": 9}),
            json!({"run_id": "42", "job_id": "24", "operation": "job_log_truncated"}),
        ],
        "sweep-b",
    );
    assert_eq!(observed.persistent.len(), 4);
    assert!(
        observed
            .persistent
            .iter()
            .all(|error| error["consecutive_sweeps"] == json!(3))
    );
    assert_eq!(observed.retryable.len(), 1);
    assert_eq!(observed.retryable[0]["run_id"], json!("42"));

    save(&path, &history).expect("save observed history");
    assert_eq!(
        fs::read_to_string(&path).expect("read observed history"),
        json!({
            "schema_version": 2,
            "counts": {
                "\"42\"/\"24\"/job_log_truncated": {"consecutive_sweeps": 1, "last_sweep_id": "sweep-b"},
                "\"run-7\"/\"99\"/logs": {"consecutive_sweeps": 3, "last_sweep_id": "sweep-b"},
                "42/24/job_log_truncated": {"consecutive_sweeps": 3, "last_sweep_id": "sweep-b"},
                "7/-/check": {"consecutive_sweeps": 3, "last_sweep_id": "sweep-b"},
                "8/9/": {"consecutive_sweeps": 3, "last_sweep_id": "sweep-b"}
            },
            "landing_gaps": {
                "branch/workflow/job": {"consecutive_sweeps": 4, "last_sweep_id": "sweep-a"}
            }
        })
        .to_string()
    );

    let streaks = history.observe_landing_gaps(
        &BTreeSet::from(["branch/workflow/job".to_string(), "bad-gap".to_string()]),
        "sweep-b",
    );
    assert_eq!(streaks["branch/workflow/job"], 5);
    assert_eq!(streaks["bad-gap"], 1);
}
