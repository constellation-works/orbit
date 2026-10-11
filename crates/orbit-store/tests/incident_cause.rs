//! Incident causes over fixture audit rows [ORB-15434]: one failure reported
//! at several surfaces shares a cause, distinct structured codes stay
//! distinct, and rows without structured evidence keep their own signature.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use chrono::DateTime;
use orbit_store::contracts::{CauseBasis, FailureIncident, group_failure_incidents};
use orbit_types::telemetry::AuditEvent;

struct Row<'a> {
    surface: &'a str,
    role: &'a str,
    run: &'a str,
    minute: i64,
    message: Option<&'a str>,
    activity: Option<&'a str>,
    denied: bool,
}

impl<'a> Row<'a> {
    fn new(surface: &'a str, run: &'a str, minute: i64, message: &'a str) -> Self {
        Self {
            surface,
            role: "codex",
            run,
            minute,
            message: Some(message),
            activity: None,
            denied: false,
        }
    }
}

/// A tool surface is a dotted name; anything else is a `command subcommand`
/// pair, the shape direct CLI and job-run lifecycle rows record.
fn event(id: i64, row: &Row<'_>) -> AuditEvent {
    let (command, subcommand, tool) = if row.surface.contains('.') {
        ("tool", Some("run"), Some(row.surface))
    } else {
        let (command, subcommand) = row.surface.split_once(' ').unwrap();
        (command, Some(subcommand), None)
    };
    serde_json::from_value(serde_json::json!({
        "id": id, "execution_id": format!("execution-{id}"),
        "timestamp": DateTime::from_timestamp(1_780_000_000 + row.minute * 60, 0).unwrap(),
        "command": command, "subcommand": subcommand, "tool_name": tool,
        "target_type": null, "target_id": null, "role": row.role,
        "status": if row.denied { "denied" } else { "failure" },
        "exit_code": 1, "duration_ms": 1, "working_directory": ".",
        "arguments_json": null, "stdout_truncated": null, "stderr_truncated": null,
        "error_message": row.message, "host": null, "pid": 1, "session_id": null,
        "job_run_id": row.run, "activity_id": row.activity,
    }))
    .unwrap()
}

fn incidents(rows: &[Row<'_>]) -> Vec<FailureIncident> {
    let events: Vec<AuditEvent> = rows
        .iter()
        .enumerate()
        .map(|(id, row)| event(id as i64 + 1, row))
        .collect();
    let incidents = group_failure_incidents(&events);
    assert_eq!(
        incidents.len(),
        rows.len(),
        "each fixture row is its own incident, so causes are compared across incidents"
    );
    incidents
}

fn on<'a>(incidents: &'a [FailureIncident], run: &str) -> &'a FailureIncident {
    incidents
        .iter()
        .find(|incident| incident.run_ids == [run])
        .unwrap()
}

const VALIDATE_FAILURE: &str =
    "required check 'make check' did not pass on candidate 9f8e7d6c5b4a: check exited 2";

#[test]
fn one_action_failure_reported_at_two_surfaces_shares_a_cause() {
    let wrapped = format!(
        "action `outer_guard` failed: child run did not succeed: action `fixture_validate` failed: {VALIDATE_FAILURE}"
    );
    let direct = format!("execution failed: action `fixture_validate` failed: {VALIDATE_FAILURE}");
    let other = "execution failed: action `fixture_validate` failed: candidate checkout is missing";
    let denied = format!("action `fixture_validate` failed: {VALIDATE_FAILURE}");
    let rows = [
        Row {
            role: "admin",
            ..Row::new("fixture.wait", "run-wait", 0, &wrapped)
        },
        Row {
            role: "human",
            ..Row::new("job worker", "run-worker", 10, &direct)
        },
        Row::new("job worker", "run-other", 20, other),
        Row {
            denied: true,
            ..Row::new("job worker", "run-denied", 30, &denied)
        },
    ];
    let incidents = incidents(&rows);
    let (wait, worker) = (on(&incidents, "run-wait"), on(&incidents, "run-worker"));

    assert_ne!(wait.signature, worker.signature, "two signatures");
    assert_eq!(
        wait.cause, worker.cause,
        "the innermost action and the message after it are one cause, whichever surface and actor reported them"
    );
    assert_eq!(wait.cause.basis, CauseBasis::StepMessage);
    assert_eq!(wait.cause.label, "action `fixture_validate`");
    assert_ne!(
        on(&incidents, "run-other").cause.key,
        wait.cause.key,
        "the same action failing with another message is another cause"
    );
    assert_ne!(
        on(&incidents, "run-denied").cause.key,
        wait.cause.key,
        "a cause never spans failure classes"
    );
    assert_eq!(
        (wait.event_count, worker.event_count),
        (1, 1),
        "sharing a cause leaves each incident's own counts alone"
    );
}

#[test]
fn distinct_codes_under_one_step_are_distinct_causes() {
    let envelope = |code: &str| {
        format!(
            "step `fixture_implement`: subprocess declared status=\"failed\" despite exit 0: error.code={code}; error.message=handoff refused"
        )
    };
    let (alpha, beta) = (envelope("alpha_unavailable"), envelope("beta_rejected"));
    let red = |base: &str| {
        format!(
            "execution failed: action `fixture_validate` failed: execution failed: [base_red] {{\"base_sha\":\"{base}\",\"command\":\"make check\"}}"
        )
    };
    let (red_one, red_two) = (red("1111aaaa2222bbbb"), red("3333cccc4444dddd"));
    let rows = [
        Row::new("fixture.wait", "run-alpha-wait", 0, &alpha),
        Row::new("job worker", "run-alpha-worker", 10, &alpha),
        Row::new("job worker", "run-beta", 20, &beta),
        Row::new("job worker", "run-red-one", 30, &red_one),
        Row::new("fixture.wait", "run-red-two", 40, &red_two),
    ];
    let incidents = incidents(&rows);
    let alpha_cause = &on(&incidents, "run-alpha-wait").cause;

    assert_eq!(alpha_cause.basis, CauseBasis::StepCode);
    assert_eq!(
        alpha_cause.label,
        "step `fixture_implement` · error.code=alpha_unavailable"
    );
    assert_eq!(
        &on(&incidents, "run-alpha-worker").cause,
        alpha_cause,
        "one code under one step is one cause at every surface"
    );
    assert_ne!(
        on(&incidents, "run-beta").cause.key,
        alpha_cause.key,
        "documented rule: a different code under the same step is a separate cause"
    );
    let (one, two) = (
        &on(&incidents, "run-red-one").cause,
        &on(&incidents, "run-red-two").cause,
    );
    assert_eq!(
        one, two,
        "a bracketed code groups operands that differ only in detail"
    );
    assert_eq!(one.label, "action `fixture_validate` · [base_red]");
}

#[test]
fn a_code_without_a_step_groups_by_the_code() {
    let rows = [
        Row::new(
            "fixture.wait",
            "run-kind-wait",
            0,
            "[blocked_by_agent] kind=sandbox_refused\nThe host refuses user namespaces.",
        ),
        Row::new(
            "job worker",
            "run-kind-worker",
            10,
            "[blocked_by_agent] kind=sandbox_refused\nnamespace probe exited 1",
        ),
        Row::new(
            "job worker",
            "run-tag",
            20,
            "[blocked_by_agent] the agent stopped without a kind",
        ),
    ];
    let incidents = incidents(&rows);
    let wait = &on(&incidents, "run-kind-wait").cause;

    assert_eq!(wait.basis, CauseBasis::Code);
    assert_eq!(wait.label, "kind=sandbox_refused");
    assert_eq!(
        wait,
        &on(&incidents, "run-kind-worker").cause,
        "a `kind=` code outranks the bracketed tag and the prose that follows it"
    );
    let tag = &on(&incidents, "run-tag").cause;
    assert_eq!(
        (tag.basis, tag.label.as_str()),
        (CauseBasis::Code, "[blocked_by_agent]")
    );
}

#[test]
fn an_activity_column_names_the_step_when_the_message_does_not() {
    let rows = [
        Row {
            activity: Some("fixture_step"),
            ..Row::new("fixture.wait", "run-col-wait", 0, "disk quota exceeded")
        },
        Row {
            activity: Some("fixture_step"),
            ..Row::new("job worker", "run-col-worker", 10, "disk quota exceeded")
        },
        Row {
            activity: Some("fixture_step"),
            message: None,
            ..Row::new("job worker", "run-col-silent", 20, "")
        },
    ];
    let incidents = incidents(&rows);
    let wait = &on(&incidents, "run-col-wait").cause;

    assert_eq!(
        (wait.basis, wait.label.as_str()),
        (CauseBasis::StepMessage, "activity `fixture_step`")
    );
    assert_eq!(wait, &on(&incidents, "run-col-worker").cause);
    assert_eq!(
        on(&incidents, "run-col-silent").cause.basis,
        CauseBasis::Signature,
        "a step with no message is no more specific than the signature"
    );
}

#[test]
fn rows_without_structured_evidence_fall_back_to_their_signature() {
    let rows = [
        Row::new(
            "fixture.read",
            "run-plain-one",
            0,
            "database connection failed",
        ),
        Row::new(
            "job worker",
            "run-plain-two",
            10,
            "database connection failed",
        ),
        Row::new(
            "job worker",
            "run-log-level",
            20,
            "[error] the `fixture_validate` check failed with code -",
        ),
    ];
    let incidents = incidents(&rows);
    for incident in &incidents {
        assert_eq!(
            incident.cause.basis,
            CauseBasis::Signature,
            "{}: a log level, an unintroduced backtick name or an empty code is not structured evidence",
            incident.signature
        );
        assert_eq!(incident.cause.label, incident.surface);
    }
    assert_ne!(
        on(&incidents, "run-plain-one").cause.key,
        on(&incidents, "run-plain-two").cause.key,
        "the same prose at two surfaces is not merged without evidence"
    );
}

#[test]
fn cause_keys_are_stable_across_repeated_computation_and_input_order() {
    let message = format!("execution failed: action `fixture_validate` failed: {VALIDATE_FAILURE}");
    let rows = [
        Row::new("fixture.wait", "run-a", 0, &message),
        Row::new("job worker", "run-b", 10, &message),
        Row::new("job worker", "run-c", 20, "database connection failed"),
    ];
    let events: Vec<AuditEvent> = rows
        .iter()
        .enumerate()
        .map(|(id, row)| event(id as i64 + 1, row))
        .collect();
    let keys = |events: &[AuditEvent]| {
        let mut keys: Vec<(String, String)> = group_failure_incidents(events)
            .into_iter()
            .map(|incident| (incident.incident_id, incident.cause.key))
            .collect();
        keys.sort();
        keys
    };
    let first = keys(&events);
    assert_eq!(first, keys(&events), "a refresh over the same rows");
    let reversed: Vec<AuditEvent> = events.iter().rev().cloned().collect();
    assert_eq!(first, keys(&reversed), "the same rows in another order");
    assert!(
        first
            .iter()
            .all(|(_, key)| key.starts_with("cause-") && key.len() == 22)
    );
}
