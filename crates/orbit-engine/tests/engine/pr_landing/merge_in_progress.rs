//! Regression for GitHub reporting an already in-flight merge (ORB-25326).
use super::*;

pub(super) fn merge_in_progress_completion_scenarios(sandbox: &Path) {
    for (case, outcome, merged_on, max_wait, expected_reads, expected_error) in [
        ("already merged", "in_progress", Some(1), 30, 2, None),
        ("merges on third read", "in_progress", Some(3), 30, 4, None),
        ("empty body", "in_progress_empty", Some(2), 30, 3, None),
        (
            "numeric status",
            "in_progress_numeric",
            Some(1),
            30,
            2,
            None,
        ),
        (
            "read ceiling",
            "in_progress",
            None,
            30,
            4,
            Some("3 read-only observations"),
        ),
        (
            "original deadline",
            "in_progress",
            None,
            5,
            2,
            Some("timed out after 5s"),
        ),
        (
            "base counter independent",
            "base_modified",
            Some(3),
            30,
            5,
            None,
        ),
    ] {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        if outcome == "base_modified" {
            fx.script_merges(&["base_modified", "in_progress"]);
        } else {
            fx.script_merges(&[outcome]);
        }
        if let Some(poll) = merged_on {
            fx.write_forge("merge-after-polls", poll.to_string());
        }
        let mut input = fx.complete_input();
        input["max_wait_seconds"] = json!(max_wait);
        let result = complete_with_logical_clock(&host, &input);
        if let Some(expected) = expected_error {
            let message = result.unwrap_err().to_string();
            assert!(message.contains(expected), "{case}: {message}");
            assert!(message.contains("stays in review"), "{case}: {message}");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
            assert!(host.landings().is_empty(), "{case}");
        } else {
            let completed = result.unwrap_or_else(|error| panic!("{case}: {error}"));
            assert_eq!(completed["merge"]["merged"], true, "{case}");
            assert_eq!(completed["merge"]["managed_merge"], false, "{case}");
            assert_eq!(
                completed["merge"]["base_modified_refusals"],
                u32::from(outcome == "base_modified"),
                "{case}"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Done, "{case}");
            assert_eq!(host.landings().len(), 1, "{case}");
            assert_eq!(
                completed["merge"]["landed_commit"],
                fx.remote_tip(BASE),
                "{case}"
            );
        }
        assert_eq!(
            fx.merge_requests().len(),
            if outcome == "base_modified" { 2 } else { 1 },
            "{case}: no second in-progress mutation"
        );
        assert_eq!(fx.status_reads(), expected_reads, "{case}");
        test_env::record_scenario_pass(case);
    }
}

pub(super) fn merge_in_progress_refusal_scenarios(sandbox: &Path) {
    let diagnostic = "gh: Merge already in progress (HTTP 405)";
    for (body, stderr) in [
        ("{malformed", diagnostic.to_string()),
        (
            r#"{"message":"Merge already in progress","status":"403"}"#,
            diagnostic.to_string(),
        ),
        (
            r#"{"message":"Bad credentials","status":"405"}"#,
            diagnostic.to_string(),
        ),
        ("", format!("{diagnostic}\ngh: Bad credentials (HTTP 401)")),
        ("", "gh: Merge already in progress (HTTP 422)".to_string()),
    ] {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.script_merges(&["in_progress_error"]);
        fx.write_forge("in-progress-body", body);
        fx.write_forge("in-progress-stderr", stderr);
        let error = complete_with_logical_clock(&host, &fx.complete_input()).unwrap_err();
        assert!(
            error.to_string().contains("could not request squash merge"),
            "{error}"
        );
        assert_eq!(fx.merge_requests().len(), 1);
        assert_eq!(fx.status_reads(), 1, "nonmatching failures never reconcile");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review);
    }
    for field in [
        "headRefOid",
        "baseRefName",
        "headRefName",
        "mergeCommit",
        "mergedAt",
        "url",
        "number",
    ] {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.script_merges(&["in_progress"]);
        let mut status = json!({"number":42,"url":"https://github.com/orbit/test/pull/42",
            "state":"MERGED","mergedAt":"2026-11-05T08:24:36Z","mergeCommit":{"oid":fx.candidate},
            "headRefName":BRANCH,"headRefOid":fx.candidate,"baseRefName":BASE});
        status[field] = Value::Null;
        fx.write_forge("reconcile-status", status.to_string());
        let error = complete_with_logical_clock(&host, &fx.complete_input()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("read-only reconciliation was inconclusive"),
            "{field}: {error}"
        );
        assert_eq!(
            fx.status_reads(),
            2,
            "{field}: refuse incomplete merged evidence"
        );
        assert_eq!(fx.merge_requests().len(), 1, "{field}: never resend");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{field}");
    }
}
