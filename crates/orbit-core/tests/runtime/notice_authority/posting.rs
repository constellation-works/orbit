//! Posting parse and lost-reply retry scenarios for fleet notices.

use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::McpCapability;
use serde_json::{Value, json};

use super::{add_task, agent_session, call, notice_audit, refused, runtime_at, session};

// Freeze the owner clock rather than sleeping across the five-minute boundary.
pub(super) fn lost_reply_retries_preserve_editor_and_corroborator_state() {
    use chrono::{Duration, Utc};
    use orbit_core::test_support;

    for editor in [true, false] {
        for relative in [true, false] {
            let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
            let runtime = runtime_at(root.path());
            let task = add_task(&runtime, "Retry report");
            let agent = agent_session(&runtime, &task);
            let operator = session(McpCapability::Operator);
            let now = Utc::now();
            let mut request = json!({"kind": "incident", "title": "Retry report",
                "linked_task": task});
            if relative {
                request["expires_in_hours"] = json!(1);
            } else {
                request["expires_at"] = json!(now + Duration::minutes(6));
            }
            if !editor {
                let mut original = request.clone();
                original["body"] = json!("Operator content");
                test_support::with_notice_post_time(now, || {
                    call(&runtime, "orbit.notice.post", original, &operator).unwrap()
                });
            }
            let posted = test_support::with_notice_post_time(now, || {
                call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap()
            });
            let id = posted["notice"]["id"].as_str().unwrap();
            let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
            let audits = notice_audit(&runtime, AuditEventStatus::Success);
            let later = now + Duration::minutes(if relative { 40 } else { 2 });
            for _ in 0..6 {
                let retry = test_support::with_notice_post_time(later, || {
                    call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap()
                });
                assert_eq!(
                    retry["outcome"],
                    if editor { "updated" } else { "corroborated" }
                );
                assert_eq!(
                    retry["notice"], posted["notice"],
                    "a retry must move no stored field"
                );
            }
            // Explicit defaults and omitted defaults identify the same request.
            let mut defaults = request.clone();
            defaults["scope"] = json!("workspace");
            defaults["dedupe_key"] = json!(format!("task:{}", task.to_lowercase()));
            defaults["close_on_linked_done"] = json!(true);
            defaults["body"] = json!("");
            let retry = test_support::with_notice_post_time(later, || {
                call(&runtime, "orbit.notice.post", defaults, &agent).unwrap()
            });
            assert_eq!(retry["notice"], posted["notice"]);
            assert_eq!(notice_audit(&runtime, AuditEventStatus::Success), audits);
            assert_eq!(
                call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap(),
                shown
            );

            let mut invalid = request.clone();
            invalid["body"] = json!("A distinct report still validates expiry");
            if relative {
                invalid["expires_in_hours"] = json!(0);
            }
            refused(
                test_support::with_notice_post_time(later, || {
                    call(&runtime, "orbit.notice.post", invalid, &agent)
                }),
                "notice_expiry_invalid",
            );

            // An intervening write, even at the same timestamp, invalidates the
            // previous request's retry identity. Never search older fingerprints.
            let other = agent_session(&runtime, &add_task(&runtime, "Other reporter"));
            let mut intervening = request.clone();
            intervening.as_object_mut().unwrap().remove("expires_at");
            intervening
                .as_object_mut()
                .unwrap()
                .remove("expires_in_hours");
            test_support::with_notice_post_time(now, || {
                call(&runtime, "orbit.notice.post", intervening, &other).unwrap()
            });
            let result = test_support::with_notice_post_time(later, || {
                call(&runtime, "orbit.notice.post", request, &agent)
            });
            if relative {
                let result = result.unwrap();
                assert_eq!(
                    result["notice"]["reports"]["count"],
                    posted["notice"]["reports"]["count"].as_u64().unwrap() + 2
                );
            } else {
                refused(result, "notice_expiry_invalid");
            }
        }
    }
}

pub(super) fn lost_reply_retries_bypass_a_reached_post_rate_only_for_the_latest_request() {
    for editor in [true, false] {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let runtime = runtime_at(root.path());
        let task = add_task(&runtime, "Rate-limited reporter");
        let agent = agent_session(&runtime, &task);
        let operator = session(McpCapability::Operator);
        let mut request = json!({"kind": "info", "title": "Rate-limited reporter",
            "linked_task": task});
        if !editor {
            call(&runtime, "orbit.notice.post", request.clone(), &operator).unwrap();
        }
        // Reach the fixed cap with distinct writes of the same open subject.
        let mut last = Value::Null;
        for report in 0..orbit_core::app::notice::AGENT_POSTS_PER_TASK_PER_DAY {
            request["body"] = json!(format!("Report {report}"));
            last = call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap();
        }
        let id = last["notice"]["id"].as_str().unwrap();
        let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
        let audits = notice_audit(&runtime, AuditEventStatus::Success);
        let retry = call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap();
        assert_eq!(
            retry["outcome"],
            if editor { "updated" } else { "corroborated" }
        );
        assert_eq!(retry["notice"], last["notice"]);
        assert_eq!(notice_audit(&runtime, AuditEventStatus::Success), audits);
        assert_eq!(
            call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap(),
            shown
        );
        // The resolved author includes run/model identity, not just the bound task.
        let mut other_run = agent.clone();
        other_run.worker_invocation.as_mut().unwrap().bound_run_id = "jrun-another-run".into();
        refused(
            call(&runtime, "orbit.notice.post", request.clone(), &other_run),
            "notice_limit_exceeded",
        );
        request["model"] = json!("codex");
        refused(
            call(&runtime, "orbit.notice.post", request.clone(), &agent),
            "notice_limit_exceeded",
        );
        request.as_object_mut().unwrap().remove("model");
        request["body"] = json!("A genuinely new report");
        refused(
            call(&runtime, "orbit.notice.post", request, &agent),
            "notice_limit_exceeded",
        );
    }
}

/// The tool surface refuses through the same core parse as HTTP: unknown
/// kind/scope, a bad `expires_at` and both expiries carry `invalid_field`,
/// while a present blank optional string and a missing kind stay refused.
pub(super) fn post_parse_refusals_come_from_core() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let operator = session(McpCapability::Operator);
    let future = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    for (input, message) in [
        (
            json!({"kind": "bogus", "title": "Bad kind"}),
            "`kind` must be warn, incident, maintenance or info",
        ),
        (
            json!({"kind": "info", "title": "Bad scope", "scope": "bogus"}),
            "`scope` must be workspace or global",
        ),
        (
            json!({"kind": "info", "title": "Bad expiry", "expires_at": "not-a-time"}),
            "`expires_at` must be an RFC 3339 timestamp",
        ),
        (
            json!({"kind": "info", "title": "Both", "expires_at": future, "expires_in_hours": 1}),
            "pass `expires_in_hours` or `expires_at`, not both",
        ),
    ] {
        let error = call(&runtime, "orbit.notice.post", input, &operator)
            .expect_err("the core parse must refuse");
        assert!(error.starts_with("notice_invalid_field"), "{error}");
        assert!(error.contains(message), "{error} does not name {message}");
    }
    for field in ["scope", "dedupe_key", "linked_task", "expires_at"] {
        let mut input = json!({"kind": "info", "title": "Blank optional field"});
        input[field] = json!("  ");
        let error = call(&runtime, "orbit.notice.post", input, &operator)
            .expect_err("a present blank optional string stays refused on the tool surface");
        assert!(
            error.starts_with("invalid input:"),
            "blank {field} keeps the tool input refusal: {error}"
        );
    }
    let error = call(
        &runtime,
        "orbit.notice.post",
        json!({"title": "Missing kind"}),
        &operator,
    )
    .expect_err("a missing kind stays refused on the tool surface");
    assert!(
        error.starts_with("invalid input:"),
        "missing kind keeps the required-field refusal: {error}"
    );
    for input in [
        json!({"kind": "info"}),
        json!({"kind": "info", "title": null}),
    ] {
        let error = call(&runtime, "orbit.notice.post", input, &operator)
            .expect_err("a missing or null title stays refused on the tool surface");
        assert!(
            error.starts_with("invalid input:"),
            "missing or null title keeps the required-field refusal: {error}"
        );
    }
    let numeric_string = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "info", "title": "Numeric string expiry", "expires_in_hours": "1"}),
        &operator,
    )
    .unwrap();
    assert_eq!(numeric_string["outcome"], "created");
}
