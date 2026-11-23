//! Regression for a merge refused because the base advanced into a conflict
//! after completion's mergeable read (F2026-11-440).
//!
//! Policy, queue, protection, auth, other-status, timeout and invalid-success
//! refusals keep their first-request error in
//! `other_merge_refusals_fail_on_the_first_request`; the base-modified and
//! merge-in-progress rows keep theirs in `base_race_fresh_gates` and
//! `merge_in_progress_contracts`. Those requests now also carry the conflict
//! opt-in.
use super::*;

const CONFLICT: &str = "gh: Pull Request has merge conflicts (HTTP 405)";

/// The exact conflict refusal takes one read, then the conflict path
/// `dispatch_conflict` already owns: a real Git conflict becomes the typed
/// recoverable conflict, a reviewed head is rebased for re-review only when
/// the pipeline asks, and otherwise ends `review_gate_stale`.
pub(super) fn merge_conflict_recovery_scenarios(sandbox: &Path) {
    // A real conflict on an ungated published head: step recovery's input.
    let (fx, host) = fx_host(sandbox);
    open_for_completion(&fx, &host);
    fx.commit_on_base(FEATURE, "base change\n");
    fx.script_checks(&["success", "dirty"]);
    fx.script_merges(&["merge_conflict"]);
    let conflict = completion_conflict(complete_with_logical_clock(
        &host,
        &completion_with_step(&fx),
    ));
    assert_eq!(
        conflict.conflicting_paths,
        vec![FEATURE.to_string()],
        "{conflict:?}"
    );
    assert_eq!(fx.merge_requests().len(), 1, "real conflict: one merge");
    assert_eq!(fx.status_reads(), 2, "real conflict: one follow-up read");
    assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "nothing pushed");
    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
    test_env::record_scenario_pass("merge-conflict-real-conflict");

    // A reviewed head the pipeline re-reviews after a rebase, for each
    // exact form of the refusal.
    for outcome in [
        "merge_conflict",
        "merge_conflict_empty",
        "merge_conflict_numeric",
    ] {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.advance_base();
        fx.script_checks(&["success", "dirty"]);
        fx.script_merges(&[outcome]);
        let mut input = fx.complete_input();
        input["re_review_on_conflict"] = json!(true);
        let rebased = complete_with_logical_clock(&host, &input)
            .unwrap_or_else(|error| panic!("{outcome}: {error}"));
        assert_eq!(rebased["re_review_required"], true, "{outcome}: {rebased}");
        assert_eq!(rebased["merge"]["merged"], false, "{outcome}");
        let rebased_head = rebased["rebased"]["head_sha"].as_str().unwrap();
        assert_eq!(rebased_head, fx.head(), "{outcome}");
        assert_ne!(rebased_head, fx.candidate, "{outcome}");
        assert_eq!(fx.merge_requests().len(), 1, "{outcome}: one merge");
        assert_eq!(fx.status_reads(), 2, "{outcome}: one follow-up read");
        assert!(host.landings().is_empty(), "{outcome}");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{outcome}");
        test_env::record_scenario_pass(&format!("merge-conflict-rebased-{outcome}"));
    }

    // A reviewed head without re-review on conflict stays stale, as a
    // polled conflict does.
    let (fx, host) = fx_host(sandbox);
    open_for_completion(&fx, &host);
    fx.advance_base();
    fx.script_checks(&["success", "dirty"]);
    fx.script_merges(&["merge_conflict"]);
    let error = complete_with_logical_clock(&host, &fx.complete_input()).unwrap_err();
    assert!(error.to_string().contains("review_gate_stale"), "{error}");
    assert_eq!(fx.merge_requests().len(), 1, "stale review: one merge");
    assert_eq!(fx.status_reads(), 2, "stale review: one follow-up read");
    assert_eq!(fx.head(), fx.candidate, "stale review: no rebase");
    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
    test_env::record_scenario_pass("merge-conflict-review-gate-stale");
}

/// Every neighbouring refusal keeps the merge failure, issues no second
/// merge and never starts conflict recovery, even when the pipeline would
/// re-review a rebase.
pub(super) fn merge_conflict_refusal_scenarios(sandbox: &Path) {
    // Not the whole exact diagnostic: no follow-up read at all.
    for (case, body, stderr) in [
        (
            "other 405 text",
            "",
            "gh: Pull Request is not mergeable (HTTP 405)",
        ),
        (
            "other status",
            "",
            "gh: Pull Request has merge conflicts (HTTP 422)",
        ),
        (
            "trailing line",
            "",
            "gh: Pull Request has merge conflicts (HTTP 405)\ngh: Bad credentials (HTTP 401)",
        ),
        (
            "leading line",
            "",
            "warning: retried after a proxy reset\ngh: Pull Request has merge conflicts (HTTP 405)",
        ),
        (
            "mixed policy text",
            "",
            "gh: Pull Request has merge conflicts (HTTP 405): Resource not accessible by integration",
        ),
        (
            "body names another message",
            r#"{"message":"Bad credentials","status":"405"}"#,
            CONFLICT,
        ),
        (
            "body names another status",
            r#"{"message":"Pull Request has merge conflicts","status":"409"}"#,
            CONFLICT,
        ),
        ("malformed body", "{malformed", CONFLICT),
    ] {
        let (fx, host) = conflict_fixture(sandbox);
        fx.script_merges(&["conflict_error"]);
        fx.write_forge("conflict-body", body);
        fx.write_forge("conflict-stderr", stderr);
        let result = complete_with_logical_clock(&host, &re_review_input(&fx));
        assert_merge_failure_kept(&fx, &host, result, case, 1);
    }
    // The follow-up read does not show the refused head still conflicting.
    for (case, outcome, follow_up) in [
        ("head changed", "merge_conflict+head_moved", "dirty"),
        ("closed", "merge_conflict", "closed"),
        ("merged", "merge_conflict+merged", "dirty"),
        ("not conflicting", "merge_conflict", "success"),
        ("blocked", "merge_conflict", "pending"),
    ] {
        let (fx, host) = conflict_fixture(sandbox);
        fx.script_checks(&["success", follow_up]);
        fx.script_merges(&[outcome]);
        let result = complete_with_logical_clock(&host, &re_review_input(&fx));
        assert_merge_failure_kept(&fx, &host, result, case, 2);
    }
    // The follow-up read itself fails.
    let (fx, host) = conflict_fixture(sandbox);
    fx.script_merges(&["merge_conflict"]);
    fx.write_forge("reconcile-status", "{}");
    fx.write_forge("reconcile-error", "gh: Not Found (HTTP 404)");
    let result = complete_with_logical_clock(&host, &re_review_input(&fx));
    assert_merge_failure_kept(&fx, &host, result, "read failure", 2);
}

/// An opened candidate whose base advanced cleanly, so a conflict path that
/// wrongly started would rebase it.
fn conflict_fixture(sandbox: &Path) -> (Fixture, DeliveryHost) {
    let (fx, host) = fx_host(sandbox);
    open_for_completion(&fx, &host);
    fx.advance_base();
    (fx, host)
}

fn re_review_input(fx: &Fixture) -> Value {
    let mut input = fx.complete_input();
    input["re_review_on_conflict"] = json!(true);
    input
}

fn assert_merge_failure_kept(
    fx: &Fixture,
    host: &DeliveryHost,
    result: Res<Value>,
    case: &str,
    reads: usize,
) {
    let message = match result {
        Err(error) => error.to_string(),
        Ok(value) => panic!("{case}: expected the merge failure, got {value}"),
    };
    assert!(
        message.contains("could not request squash merge"),
        "{case}: {message}"
    );
    assert!(message.contains("stays in review"), "{case}: {message}");
    // Only the exact refusal reaches the follow-up read, and every answer it
    // gets here keeps the merge failure.
    assert_eq!(
        message.contains("the follow-up read"),
        reads == 2,
        "{case}: {message}"
    );
    assert_eq!(fx.merge_requests().len(), 1, "{case}: no second merge");
    assert_eq!(fx.status_reads(), reads, "{case}: reads");
    assert_eq!(fx.head(), fx.candidate, "{case}: no conflict rebase");
    assert!(host.landings().is_empty(), "{case}");
    assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
    test_env::record_scenario_pass(&format!("merge-conflict-kept-{case}"));
}
