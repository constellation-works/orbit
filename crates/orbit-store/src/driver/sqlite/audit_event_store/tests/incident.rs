//! Failure-incident grouping contract [ORB-10871].
//!
//! Every fixture below is synthetic: tool names, roles, run ids, and messages
//! are generic placeholders, so the contract these tests pin is the grouping
//! *shape*, never a value observed in one workspace.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};

use crate::Store;
use crate::driver::sqlite::audit_event_store::incident::{
    CASCADE_WINDOW_SECS, FailureClass, FailureIncidentQuery, build_report, group_failure_incidents,
    has_tool_identity, is_failure_only_diagnostic_surface, is_lifecycle_diagnostic,
    normalize_message, signature_for,
};

use crate::AuditEventFilter;
use crate::contracts::AuditEventInsertParams;

fn base_ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2031, 3, 4, 5, 0, 0)
        .single()
        .expect("fixed test timestamp")
}

struct FailureFixture {
    id: i64,
    offset_secs: i64,
    tool: Option<&'static str>,
    command: &'static str,
    role: &'static str,
    status: AuditEventStatus,
    message: Option<String>,
    run_id: Option<&'static str>,
    activity_id: Option<&'static str>,
    task_id: Option<&'static str>,
}

impl FailureFixture {
    fn new(id: i64, offset_secs: i64, tool: &'static str) -> Self {
        Self {
            id,
            offset_secs,
            tool: Some(tool),
            command: "tool",
            role: "actor-one",
            status: AuditEventStatus::Failure,
            message: Some("operation failed".to_string()),
            run_id: None,
            activity_id: None,
            task_id: Some("REC-1"),
        }
    }

    fn no_tool(mut self) -> Self {
        self.tool = None;
        self
    }

    fn command(mut self, command: &'static str) -> Self {
        self.command = command;
        self
    }

    fn role(mut self, role: &'static str) -> Self {
        self.role = role;
        self
    }

    fn status(mut self, status: AuditEventStatus) -> Self {
        self.status = status;
        self
    }

    fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    fn run(mut self, run_id: &'static str, activity_id: &'static str) -> Self {
        self.run_id = Some(run_id);
        self.activity_id = Some(activity_id);
        self
    }

    fn build(self) -> AuditEvent {
        AuditEvent {
            id: self.id,
            execution_id: format!("exec-{}", self.id),
            timestamp: base_ts() + Duration::seconds(self.offset_secs),
            command: self.command.to_string(),
            subcommand: Some("run".to_string()),
            tool_name: self.tool.map(str::to_string),
            target_type: Some(
                if self.tool.is_some() {
                    "tool"
                } else {
                    "job_run"
                }
                .to_string(),
            ),
            target_id: self
                .tool
                .map(str::to_string)
                .or_else(|| self.run_id.map(str::to_string)),
            role: self.role.to_string(),
            status: self.status,
            exit_code: 1,
            duration_ms: 5,
            working_directory: "/workdir".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: self.message,
            host: None,
            pid: 1,
            session_id: None,
            workspace_id: Some("ws-one".to_string()),
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: BTreeSet::new(),
            origin_session_id: None,
            mcp_call_id: None,
            trace_id: None,
            caller_ip: None,
            lease_id: None,
            task_id: self.task_id.map(str::to_string),
            job_run_id: self.run_id.map(str::to_string),
            activity_id: self.activity_id.map(str::to_string),
            step_index: None,
            self_reported_actor: None,
            plugin: None,
            plugin_secrets: Vec::new(),
        }
    }
}

#[test]
fn burst_of_identical_failures_collapses_into_one_incident_carrying_its_raw_count() {
    // 300 refusals of the same shape, differing only in the operand path — the
    // exact pattern that made a single burst read as hundreds of failures.
    let failures: Vec<AuditEvent> = (0..300)
        .map(|index| {
            FailureFixture::new(index, index, "surface.alpha")
                .message(format!("could not remove /work/dir-{index}/file.txt"))
                .build()
        })
        .collect();

    let report = build_report(&failures, false);

    assert_eq!(report.incident_count(), 1, "one burst is one incident");
    assert_eq!(report.raw_failed_events, 300, "no raw evidence is dropped");
    let incident = &report.incidents[0];
    assert_eq!(incident.event_count, 300);
    assert_eq!(incident.root_event_count, 300);
    assert_eq!(incident.first_ts, base_ts());
    assert_eq!(incident.last_ts, base_ts() + Duration::seconds(299));
    assert!(
        incident.signature.contains("<path>"),
        "the volatile operand must be normalized out of the signature: {}",
        incident.signature
    );
    assert!(
        incident.sample_events.len() <= 20 && !incident.sample_events.is_empty(),
        "a bounded sample of the underlying rows must be reachable"
    );
}

#[test]
fn unrelated_failures_remain_separate_incidents() {
    let failures = vec![
        FailureFixture::new(1, 0, "surface.alpha")
            .message("could not remove /work/a.txt")
            .build(),
        FailureFixture::new(2, 1, "surface.beta")
            .message("connection reset by peer")
            .build(),
        FailureFixture::new(3, 2, "surface.alpha")
            .role("actor-two")
            .message("could not remove /work/b.txt")
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(
        report.incident_count(),
        3,
        "different surface, message, or actor must not be merged"
    );
    assert!(report.incidents.iter().all(|i| i.event_count == 1));
}

#[test]
fn cascading_run_failures_expose_one_root_with_the_chain_collapsed_beneath_it() {
    // One failed run whose failure propagates up three enclosing steps. Each
    // layer writes its own audit row; only the first is a root cause.
    let failures = vec![
        FailureFixture::new(1, 0, "step.inner")
            .message("child step returned a nonzero status")
            .run("run-one", "step-inner")
            .build(),
        FailureFixture::new(2, 1, "step.middle")
            .message("bundle aborted after a child failure")
            .run("run-one", "step-middle")
            .build(),
        FailureFixture::new(3, 2, "step.gate")
            .message("gate refused to advance")
            .run("run-one", "step-gate")
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(report.incident_count(), 1, "one failed run is one incident");
    let incident = &report.incidents[0];
    assert_eq!(
        incident.surface, "step.inner",
        "the earliest failure is the root, not the outermost wrapper"
    );
    assert_eq!(incident.event_count, 3, "raw rows stay counted");
    assert_eq!(incident.root_event_count, 1);
    assert_eq!(incident.propagated_event_count(), 2);
    let chain: Vec<&str> = incident
        .propagation
        .iter()
        .map(|link| link.surface.as_str())
        .collect();
    assert_eq!(
        chain,
        vec!["step.middle", "step.gate"],
        "the propagation chain is ordered by occurrence"
    );
    assert_eq!(incident.run_ids, vec!["run-one".to_string()]);
}

#[test]
fn a_later_unrelated_failure_in_the_same_run_is_not_folded_into_the_cascade() {
    let failures = vec![
        FailureFixture::new(1, 0, "step.inner")
            .message("child step returned a nonzero status")
            .run("run-one", "step-inner")
            .build(),
        FailureFixture::new(2, 1, "step.middle")
            .message("bundle aborted after a child failure")
            .run("run-one", "step-middle")
            .build(),
        FailureFixture::new(3, CASCADE_WINDOW_SECS * 3, "step.later")
            .message("unrelated downstream problem")
            .run("run-one", "step-later")
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(
        report.incident_count(),
        2,
        "a failure beyond the cascade window is its own root cause"
    );
    let cascade = report
        .incidents
        .iter()
        .find(|incident| incident.surface == "step.inner")
        .expect("cascade incident");
    assert_eq!(cascade.event_count, 2);
    let later = report
        .incidents
        .iter()
        .find(|incident| incident.surface == "step.later")
        .expect("independent incident");
    assert!(later.propagation.is_empty());
}

#[test]
fn denials_expected_negative_paths_and_unexpected_failures_never_merge() {
    let failures = vec![
        FailureFixture::new(1, 0, "surface.alpha")
            .status(AuditEventStatus::Denied)
            .message("policy denied: write outside the allowed scope")
            .run("run-one", "step-one")
            .build(),
        FailureFixture::new(2, 1, "surface.alpha")
            .message("invalid input: field must not be empty")
            .run("run-one", "step-one")
            .build(),
        FailureFixture::new(3, 2, "surface.alpha")
            .message("internal channel closed unexpectedly")
            .run("run-one", "step-one")
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(report.incident_count(), 3);
    let classes: BTreeSet<FailureClass> = report
        .incidents
        .iter()
        .map(|incident| incident.class)
        .collect();
    assert_eq!(
        classes,
        BTreeSet::from([
            FailureClass::Denied,
            FailureClass::Expected,
            FailureClass::Unexpected
        ]),
        "same run and same surface must not blur the three failure classes"
    );
    assert_eq!(report.raw_events_by_class.get("denied"), Some(&1));
    assert_eq!(report.raw_events_by_class.get("expected"), Some(&1));
    assert_eq!(report.raw_events_by_class.get("unexpected"), Some(&1));
    assert_eq!(report.incidents_by_class.get("unexpected"), Some(&1));
}

#[test]
fn duplicate_worker_diagnostics_are_seven_incidents_not_fourteen_tool_failures() {
    const RUN_IDS: [&str; 7] = [
        "jrun-diagnostic-0",
        "jrun-diagnostic-1",
        "jrun-diagnostic-2",
        "jrun-diagnostic-3",
        "jrun-diagnostic-4",
        "jrun-diagnostic-5",
        "jrun-diagnostic-6",
    ];
    let mut failures = Vec::new();
    for (index, run_id) in RUN_IDS.into_iter().enumerate() {
        for duplicate in 0..2 {
            failures.push(
                FailureFixture::new(
                    (index * 2 + duplicate) as i64,
                    (index * 10 + duplicate) as i64,
                    "pipeline.worker.exit",
                )
                .message("pipeline worker exited after claiming the persisted run")
                .run(run_id, "worker-observer")
                .build(),
            );
        }
    }

    let report = build_report(&failures, false);

    assert!(is_failure_only_diagnostic_surface("pipeline.worker.exit"));
    assert!(failures.iter().all(is_lifecycle_diagnostic));
    assert_eq!(report.raw_failed_events, 14);
    assert_eq!(report.incident_count(), 7);
    assert_eq!(report.lifecycle_diagnostic_events, 14);
    assert_eq!(report.lifecycle_diagnostic_incidents, 7);
    assert_eq!(report.lifecycle_diagnostic_affected_run_count, 7);
    assert_eq!(report.raw_events_by_class.get("diagnostic"), Some(&14));
    assert_eq!(report.incidents_by_class.get("diagnostic"), Some(&7));
    assert_eq!(report.affected_runs_by_class.get("diagnostic"), Some(&7));
    assert!(report.incidents.iter().all(|incident| {
        incident.class == FailureClass::Diagnostic
            && incident.event_count == 2
            && incident.events.len() == 2
            && incident.events.iter().all(|event| {
                event.tool_name.as_deref() == Some("pipeline.worker.exit")
                    && event.run_id.is_some()
                    && event.task_id.is_some()
            })
    }));
}

#[test]
fn direct_cli_failure_is_unexpected_while_no_tool_start_is_diagnostic() {
    let mut direct_success = FailureFixture::new(0, 0, "unused")
        .no_tool()
        .command("run")
        .status(AuditEventStatus::Success)
        .message("operation completed")
        .build();
    direct_success.subcommand = Some("show".to_string());
    direct_success.target_id = Some("jrun-direct".to_string());

    let mut direct_failure = FailureFixture::new(1, 0, "unused")
        .no_tool()
        .command("run")
        .message("storage backend disconnected")
        .build();
    direct_failure.subcommand = Some("show".to_string());
    direct_failure.target_id = Some("jrun-direct".to_string());

    let mut lifecycle_failure = FailureFixture::new(2, 1, "unused")
        .no_tool()
        .command("Start")
        .message("job run start failed")
        .build();
    lifecycle_failure.subcommand = None;

    assert!(!is_lifecycle_diagnostic(&direct_success));
    assert!(!is_lifecycle_diagnostic(&direct_failure));
    assert!(is_lifecycle_diagnostic(&lifecycle_failure));

    let report = build_report(&[direct_failure, lifecycle_failure], false);

    assert_eq!(report.raw_events_by_class.get("unexpected"), Some(&1));
    assert_eq!(report.raw_events_by_class.get("diagnostic"), Some(&1));

    let direct = report
        .incidents
        .iter()
        .find(|incident| incident.surface == "run show")
        .expect("direct CLI incident");
    assert_eq!(direct.class, FailureClass::Unexpected);
    assert!(!direct.has_tool_identity);
    assert_eq!(direct.events.len(), 1);
    assert_eq!(direct.events[0].id, 1);
    assert_eq!(direct.events[0].execution_id, "exec-1");
    assert_eq!(direct.events[0].surface, "run show");
    assert_eq!(direct.events[0].tool_name, None);
    assert_eq!(
        direct.events[0].message.as_deref(),
        Some("storage backend disconnected")
    );

    let lifecycle = report
        .incidents
        .iter()
        .find(|incident| incident.surface == "Start")
        .expect("pipeline lifecycle incident");
    assert_eq!(lifecycle.class, FailureClass::Diagnostic);
    assert!(!lifecycle.has_tool_identity);
    assert_eq!(lifecycle.events.len(), 1);
    assert_eq!(lifecycle.events[0].id, 2);
    assert_eq!(lifecycle.events[0].execution_id, "exec-2");
    assert_eq!(lifecycle.events[0].surface, "Start");
    assert_eq!(lifecycle.events[0].tool_name, None);
    assert_eq!(
        lifecycle.events[0].message.as_deref(),
        Some("job run start failed")
    );
}

#[test]
fn expected_task_show_and_update_negatives_do_not_enter_unexpected_counts() {
    let failures = vec![
        FailureFixture::new(1, 0, "orbit.task.show")
            .message("not found: task REC-missing")
            .build(),
        FailureFixture::new(2, 1, "orbit.task.update")
            .message("unsupported field projection: mystery")
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(report.raw_events_by_class.get("expected"), Some(&2));
    assert_eq!(report.incidents_by_class.get("expected"), Some(&2));
    assert_eq!(report.raw_events_by_class.get("unexpected"), None);
    assert!(report.incidents.iter().all(|incident| {
        incident.class == FailureClass::Expected
            && incident.events.len() == 1
            && incident.events[0].tool_name.is_some()
    }));
}

#[test]
fn companion_not_installed_is_an_expected_negative_not_an_unexpected_failure() {
    let failures = vec![
        FailureFixture::new(1, 0, "semantic.index")
            .message(
                "companion not installed: Semantic search not enabled. Run `orbit semantic \
                 install` to download the inference companion.",
            )
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(report.raw_events_by_class.get("expected"), Some(&1));
    assert_eq!(report.incidents_by_class.get("expected"), Some(&1));
    assert_eq!(report.raw_events_by_class.get("unexpected"), None);
    assert_eq!(report.incidents[0].class, FailureClass::Expected);
}

#[test]
fn a_denial_recorded_with_failure_status_still_classifies_as_a_denial() {
    let failures = vec![
        FailureFixture::new(1, 0, "surface.alpha")
            .message("capability denied: caller lacks the required capability")
            .build(),
    ];

    let report = build_report(&failures, false);

    assert_eq!(report.incidents[0].class, FailureClass::Denied);
}

#[test]
fn grouping_is_order_independent_and_ids_are_stable() {
    let mut failures = vec![
        FailureFixture::new(1, 0, "surface.alpha")
            .message("could not remove /work/a.txt")
            .build(),
        FailureFixture::new(2, 5, "surface.beta")
            .message("connection reset by peer")
            .run("run-one", "step-one")
            .build(),
        FailureFixture::new(3, 6, "surface.gamma")
            .message("aborting after upstream failure")
            .run("run-one", "step-two")
            .build(),
    ];

    let first = group_failure_incidents(&failures);
    failures.reverse();
    let second = group_failure_incidents(&failures);

    assert_eq!(first, second, "input order must not change the grouping");
    assert!(
        first
            .iter()
            .all(|incident| incident.incident_id.starts_with("inc-")),
        "every incident carries a deterministic handle"
    );
}

#[test]
fn message_normalization_replaces_volatile_tokens_only() {
    assert_eq!(
        normalize_message("could not remove /work/dir-9/file.txt after 4 tries"),
        "could not remove <path> after <num> tries"
    );
    assert_eq!(
        normalize_message("run REC-42 at 2031-03-04T05:00:00Z failed"),
        "run <id> at <ts> failed"
    );
    assert_eq!(
        normalize_message("digest 0a1b2c3d4e5f6789 mismatch"),
        "digest <hex> mismatch"
    );
    assert_eq!(
        normalize_message("session 123e4567-e89b-12d3-a456-426614174000 expired"),
        "session <uuid> expired"
    );
}

#[test]
fn a_failure_without_a_message_groups_on_its_exit_code() {
    let event = FailureFixture::new(1, 0, "surface.alpha");
    let mut event = event.build();
    event.error_message = None;

    let signature = signature_for(&event);

    assert!(
        signature.contains("exit=1"),
        "an empty message must still yield a usable signature: {signature}"
    );
}

#[test]
fn the_store_query_groups_failures_without_hiding_any_audit_row() {
    let store = Store::open_in_memory().expect("open store");
    let insert = |execution_id: &str, status: AuditEventStatus, message: Option<&str>| {
        store
            .insert_audit_event_record(&AuditEventInsertParams {
                execution_id: execution_id.to_string(),
                command: "tool".to_string(),
                subcommand: Some("run".to_string()),
                tool_name: Some("surface.alpha".to_string()),
                target_type: Some("tool".to_string()),
                target_id: Some("surface.alpha".to_string()),
                role: "actor-one".to_string(),
                status,
                exit_code: if matches!(status, AuditEventStatus::Success) {
                    0
                } else {
                    1
                },
                duration_ms: 3,
                working_directory: "/workdir".to_string(),
                arguments_json: None,
                stdout_truncated: None,
                stderr_truncated: None,
                error_message: message.map(str::to_string),
                host: None,
                pid: 1,
                session_id: None,
                workspace_id: Some("ws-one".to_string()),
                caller_machine_id: None,
                caller_machine_name: None,
                process_machine_id: None,
                process_machine_name: None,
                transport: None,
                effective_capabilities: BTreeSet::new(),
                origin_session_id: None,
                mcp_call_id: None,
                lease_id: None,
                task_id: None,
                job_run_id: None,
                activity_id: None,
                step_index: None,
            })
            .expect("insert audit event");
    };

    insert("exec-ok", AuditEventStatus::Success, None);
    for index in 0..5 {
        insert(
            &format!("exec-fail-{index}"),
            AuditEventStatus::Failure,
            Some(&format!("could not remove /work/dir-{index}/file.txt")),
        );
    }
    insert(
        "exec-denied",
        AuditEventStatus::Denied,
        Some("policy denied: write outside the allowed scope"),
    );

    let report = store
        .get_failure_incidents(&FailureIncidentQuery::default())
        .expect("failure incidents");

    assert_eq!(report.raw_failed_events, 6, "success rows are not counted");
    assert_eq!(
        report.incident_count(),
        2,
        "the burst collapses; the denial stays separate"
    );
    assert!(!report.truncated);

    let all_rows = store
        .list_audit_events(&AuditEventFilter {
            limit: 100,
            ..Default::default()
        })
        .expect("raw audit rows");
    assert_eq!(
        all_rows.len(),
        7,
        "grouping must not remove anything from the raw audit view"
    );
}

#[test]
fn a_truncated_scan_is_reported_rather_than_silently_capped() {
    let store = Store::open_in_memory().expect("open store");
    for index in 0..3 {
        store
            .insert_audit_event_record(&AuditEventInsertParams {
                execution_id: format!("exec-{index}"),
                command: "tool".to_string(),
                subcommand: Some("run".to_string()),
                tool_name: Some("surface.alpha".to_string()),
                target_type: Some("tool".to_string()),
                target_id: Some("surface.alpha".to_string()),
                role: "actor-one".to_string(),
                status: AuditEventStatus::Failure,
                exit_code: 1,
                duration_ms: 3,
                working_directory: "/workdir".to_string(),
                arguments_json: None,
                stdout_truncated: None,
                stderr_truncated: None,
                error_message: Some("operation failed".to_string()),
                host: None,
                pid: 1,
                session_id: None,
                workspace_id: None,
                caller_machine_id: None,
                caller_machine_name: None,
                process_machine_id: None,
                process_machine_name: None,
                transport: None,
                effective_capabilities: BTreeSet::new(),
                origin_session_id: None,
                mcp_call_id: None,
                lease_id: None,
                task_id: None,
                job_run_id: None,
                activity_id: None,
                step_index: None,
            })
            .expect("insert audit event");
    }

    let report = store
        .get_failure_incidents(&FailureIncidentQuery {
            max_events: 2,
            ..Default::default()
        })
        .expect("failure incidents");

    assert!(report.truncated, "a capped scan must say so");
    assert_eq!(report.raw_failed_events, 2);
}

/// ORB-10969: ten no-tool rows that the dashboard used to label `unknown`.
/// Two are duplicate Start events; the other eight are parent/child guard
/// copies of four leaf failures. Grouping must not treat them as ten tool
/// failures or as eight independent roots.
fn ten_unknown_lifecycle_rows() -> Vec<AuditEvent> {
    let start = |id: i64, offset_secs: i64| {
        let mut event = FailureFixture::new(id, offset_secs, "unused")
            .no_tool()
            .command("Start")
            .message("job run start failed")
            .build();
        event.subcommand = None;
        event
    };
    let mut failures = vec![start(1, 0), start(2, 1)];
    for index in 0..4 {
        let leaf = format!("jrun-leaf-{index}");
        let parent = format!("jrun-parent-{index}");
        let leaf_id = 10 + index;
        let parent_id = 20 + index;
        let offset = 10 + index * 2;
        let mut leaf_event = FailureFixture::new(leaf_id, offset, "unused")
            .no_tool()
            .command("job")
            .message("child step returned a nonzero status")
            .build();
        leaf_event.job_run_id = Some(leaf.clone());
        leaf_event.activity_id = Some("leaf-step".to_string());
        leaf_event.task_id = Some(format!("REC-leaf-{index}"));
        let mut parent_event = FailureFixture::new(parent_id, offset + 1, "unused")
            .no_tool()
            .command("job")
            .message(format!(
                "pipeline child run did not succeed: result run {leaf} status failed: child step returned a nonzero status"
            ))
            .build();
        parent_event.job_run_id = Some(parent);
        parent_event.activity_id = Some("pipeline_success_guard".to_string());
        parent_event.task_id = Some(format!("REC-parent-{index}"));
        failures.push(leaf_event);
        failures.push(parent_event);
    }
    failures
}

#[test]
fn ten_unknown_rows_keep_diagnostics_to_the_failure_only_start_surface() {
    let failures = ten_unknown_lifecycle_rows();
    assert_eq!(failures.len(), 10);
    assert!(
        failures.iter().all(|event| !has_tool_identity(event)),
        "the regression fixture is the no-tool-identity population"
    );

    let report = build_report(&failures, false);

    assert_eq!(report.raw_failed_events, 10);
    assert_eq!(
        report.job_run_lifecycle_events, 10,
        "no-tool rows are job-run lifecycle events, not tool `unknown`"
    );
    assert_eq!(
        report.incident_count(),
        5,
        "2 duplicate Starts collapse; 8 cascade rows from 4 leaves are 4 roots"
    );
    assert_eq!(report.job_run_lifecycle_incidents, 5);
    assert_eq!(report.lifecycle_diagnostic_events, 2);
    assert_eq!(report.lifecycle_diagnostic_incidents, 1);
    assert_eq!(report.lifecycle_diagnostic_affected_run_count, 0);
    assert_eq!(report.raw_events_by_class.get("diagnostic"), Some(&2));
    assert_eq!(report.raw_events_by_class.get("unexpected"), Some(&8));
    assert_eq!(
        report.affected_run_count, 8,
        "4 leaf runs + 4 parent runs; Starts carry no run id"
    );

    let start = report
        .incidents
        .iter()
        .find(|incident| incident.surface == "Start")
        .expect("duplicate Start incident");
    assert_eq!(start.event_count, 2);
    assert_eq!(start.class, FailureClass::Diagnostic);
    assert!(start.propagation.is_empty());
    assert!(!start.has_tool_identity);
    assert_eq!(start.events.len(), 2);

    let cascades: Vec<_> = report
        .incidents
        .iter()
        .filter(|incident| incident.surface != "Start")
        .collect();
    assert_eq!(cascades.len(), 4);
    for incident in cascades {
        assert_eq!(incident.class, FailureClass::Unexpected);
        assert_eq!(incident.root_event_count, 1);
        assert_eq!(
            incident.propagated_event_count(),
            1,
            "each leaf's parent guard is cascade, not a second root: {}",
            incident.signature
        );
        assert_eq!(incident.event_count, 2);
        assert_eq!(incident.propagation.len(), 1);
        assert_eq!(incident.run_ids.len(), 2);
        assert_eq!(
            incident.events.len(),
            2,
            "expansion must keep every underlying row"
        );
        assert!(
            incident
                .events
                .iter()
                .all(|event| event.tool_name.is_none() && event.run_id.is_some()),
            "every cascade row carries run identity and no tool name"
        );
        assert!(
            incident.events.iter().any(|event| event
                .task_id
                .as_deref()
                .is_some_and(|id| id.starts_with("REC-"))),
            "task identifiers stay on the expanded rows"
        );
    }
}

#[test]
fn the_store_query_categorizes_the_ten_unknown_lifecycle_rows() {
    let store = Store::open_in_memory().expect("open store");
    for event in ten_unknown_lifecycle_rows() {
        store
            .insert_audit_event_record(&AuditEventInsertParams {
                execution_id: event.execution_id.clone(),
                command: event.command.clone(),
                subcommand: event.subcommand.clone(),
                tool_name: event.tool_name.clone(),
                target_type: event.target_type.clone(),
                target_id: event.target_id.clone(),
                role: event.role.clone(),
                status: event.status,
                exit_code: event.exit_code,
                duration_ms: event.duration_ms,
                working_directory: event.working_directory.clone(),
                arguments_json: event.arguments_json.clone(),
                stdout_truncated: None,
                stderr_truncated: None,
                error_message: event.error_message.clone(),
                host: event.host.clone(),
                pid: event.pid,
                session_id: event.session_id.clone(),
                workspace_id: event.workspace_id.clone(),
                caller_machine_id: None,
                caller_machine_name: None,
                process_machine_id: None,
                process_machine_name: None,
                transport: None,
                effective_capabilities: BTreeSet::new(),
                origin_session_id: None,
                mcp_call_id: None,
                lease_id: None,
                task_id: event.task_id.clone(),
                job_run_id: event.job_run_id.clone(),
                activity_id: event.activity_id.clone(),
                step_index: event.step_index,
            })
            .expect("insert lifecycle audit event");
    }

    let report = store
        .get_failure_incidents(&FailureIncidentQuery::default())
        .expect("failure incidents");

    assert_eq!(report.raw_failed_events, 10);
    assert_eq!(report.incident_count(), 5);
    assert_eq!(report.job_run_lifecycle_events, 10);
    assert_eq!(report.job_run_lifecycle_incidents, 5);
    assert_eq!(report.affected_run_count, 8);
}
