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
