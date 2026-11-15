//! [ORB-23164] A sweep GitHub rate-limited defers to the reset instead of
//! failing as a retryable run, and reads nothing again before the reset.

use serde_json::json;

use super::sweep::{Workspace, isolated_with_fake_gh, open_sweep_tasks, unattributed};

#[test]
fn ci_sweep_defers_a_rate_limited_collection_to_the_reset() {
    if !isolated_with_fake_gh(
        "sweep_rate_limit::ci_sweep_defers_a_rate_limited_collection_to_the_reset",
    ) {
        return;
    }
    let mut workspace = Workspace::new();
    workspace.push_red_run(9_100, 91_000, &unattributed("error: tests failed"));
    let reset = chrono::Utc::now().timestamp() + 3_600;
    std::fs::write(workspace.state.join("rate-limited"), reset.to_string()).unwrap();

    let (evidence, filed) = workspace.sweep();
    assert_eq!(evidence["outcome_hint"], "rate_limited", "{evidence:#}");
    assert_eq!(evidence["collected"], false, "{evidence:#}");
    let rate_limit = &evidence["rate_limit"];
    assert_eq!(
        rate_limit["reset_epoch_seconds"],
        json!(reset),
        "{evidence:#}"
    );
    assert_eq!(
        rate_limit["not_before_ms"],
        json!(reset * 1000),
        "{evidence:#}"
    );
    let output = filed.expect("a rate-limited sweep is deferred, not a failed run");
    assert_eq!(output["outcome"], "deferred", "{output:#}");
    assert_eq!(output["rate_limit"]["not_before_ms"], json!(reset * 1000));
    assert!(
        output["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("rate-limited")),
        "{output:#}"
    );
    assert!(open_sweep_tasks(&workspace.runtime).is_empty());

    // GitHub answers again, but the recorded reset has not passed: the next
    // sweep defers without spending any more of the quota.
    std::fs::remove_file(workspace.state.join("rate-limited")).unwrap();
    let calls = std::fs::read_to_string(workspace.state.join("calls")).unwrap();
    let (evidence, filed) = workspace.sweep();
    assert_eq!(evidence["outcome_hint"], "rate_limited", "{evidence:#}");
    assert_eq!(
        evidence["rate_limit"]["not_before_ms"],
        json!(reset * 1000),
        "{evidence:#}"
    );
    assert_eq!(filed.expect("still deferred")["outcome"], "deferred");
    assert_eq!(
        std::fs::read_to_string(workspace.state.join("calls")).unwrap(),
        calls,
        "a sweep before the reset reads nothing from GitHub"
    );
}
