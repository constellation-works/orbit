//! Self-reported actor persistence and the attribution split (ORB-10890).

use crate::AuditAttributionAggregate;
use crate::AuditEventFilter;
use crate::AuditEventInsertParams;
use crate::AuditInvocationFields;
use crate::Store;
use chrono::{Duration, Utc};
use orbit_types::telemetry::AuditAttribution;
use orbit_types::telemetry::AuditEventStatus;
use std::collections::BTreeSet;

fn tool_call_params(execution_id: &str, role: &str) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: execution_id.to_string(),
        command: "tool".to_string(),
        subcommand: Some("run-mcp".to_string()),
        tool_name: Some("orbit.task.show".to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some("orbit.task.show".to_string()),
        role: role.to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 1,
        working_directory: "/tmp".to_string(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
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
    }
}

fn insert(store: &Store, execution_id: &str, role: &str, claim: Option<&str>) {
    store
        .insert_audit_event_record_with_invocation(
            &tool_call_params(execution_id, role),
            AuditInvocationFields {
                self_reported_actor: claim,
                ..AuditInvocationFields::default()
            },
        )
        .expect("insert audit event");
}

fn since() -> chrono::DateTime<Utc> {
    Utc::now() - Duration::hours(1)
}

fn bucket<'a>(
    aggregates: &'a [AuditAttributionAggregate],
    attribution: AuditAttribution,
    actor: &str,
) -> &'a AuditAttributionAggregate {
    aggregates
        .iter()
        .find(|row| row.attribution == attribution && row.actor == actor)
        .unwrap_or_else(|| panic!("missing {attribution}/{actor} in {aggregates:?}"))
}

#[test]
fn an_unauthenticated_call_records_its_claim_without_changing_its_role() {
    let store = Store::open_in_memory().expect("open store");
    insert(&store, "exec-1", "unverified", Some("claude-code"));

    let events = store
        .list_audit_events(&AuditEventFilter::default())
        .expect("list audit events");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].role, "unverified");
    assert_eq!(
        events[0].self_reported_actor.as_deref(),
        Some("claude-code")
    );
    // The trusted projection is derived from `role` alone, so the claim
    // cannot move the row into an attributed actor bucket.
    assert_eq!(events[0].actor().kind.as_str(), "unattributed");
    assert_eq!(events[0].actor().id, "unverified");
}

#[test]
fn a_claim_never_reaches_the_trusted_actor_columns() {
    let store = Store::open_in_memory().expect("open store");
    insert(&store, "exec-1", "unverified", Some("admin"));

    let conn = store.read().expect("read connection");
    let (role, kind, id, claim): (String, String, String, String) = conn
        .query_row(
            "SELECT role, actor_kind, actor_id, self_reported_actor FROM audit_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read row");

    // A caller claiming the most privileged label Orbit has still lands in the
    // untrusted column, and every trusted column reads as before.
    assert_eq!(claim, "admin");
    assert_eq!(role, "unverified");
    assert_eq!(kind, "unattributed");
    assert_eq!(id, "unverified");
}

#[test]
fn attribution_split_reports_authenticated_self_reported_and_combined() {
    let store = Store::open_in_memory().expect("open store");
    insert(&store, "exec-1", "claude-opus-5", None);
    insert(&store, "exec-2", "opus", None);
    insert(&store, "exec-3", "unverified", Some("claude-code"));
    insert(&store, "exec-4", "unverified", Some("claude-code"));
    insert(&store, "exec-5", "unverified", Some("codex"));
    insert(&store, "exec-6", "unverified", None);

    let aggregates = store
        .get_audit_tool_call_counts_by_attribution(Some(&since()))
        .expect("attribution aggregate");

    // Authenticated traffic still collapses to one canonical actor.
    assert_eq!(
        bucket(&aggregates, AuditAttribution::Authenticated, "claude").total,
        2
    );
    assert_eq!(
        bucket(&aggregates, AuditAttribution::SelfReported, "claude-code").total,
        2
    );
    assert_eq!(
        bucket(&aggregates, AuditAttribution::SelfReported, "codex").total,
        1
    );
    assert_eq!(
        bucket(&aggregates, AuditAttribution::Anonymous, "anonymous").total,
        1
    );

    let sum = |attribution: AuditAttribution| -> u64 {
        aggregates
            .iter()
            .filter(|row| row.attribution == attribution)
            .map(|row| row.total)
            .sum()
    };
    assert_eq!(sum(AuditAttribution::Authenticated), 2);
    assert_eq!(sum(AuditAttribution::SelfReported), 3);
    assert_eq!(sum(AuditAttribution::Anonymous), 1);
    // Disjoint: the combined denominator is the sum, not an overlap.
    let combined: u64 = aggregates.iter().map(|row| row.total).sum();
    assert_eq!(combined, 6);

    // And it matches the role-grouped denominator over the same rows, so the
    // new view neither drops nor double-counts anything.
    let by_role: u64 = store
        .get_audit_tool_call_counts_by_role(Some(&since()))
        .expect("role aggregate")
        .iter()
        .map(|row| row.total)
        .sum();
    assert_eq!(combined, by_role);
}

#[test]
fn a_self_reported_claim_does_not_merge_with_the_authenticated_actor_it_names() {
    // The same agent, half of whose traffic Orbit can authenticate. Two rows,
    // not one: merging them would publish unverifiable calls as measured ones.
    let store = Store::open_in_memory().expect("open store");
    insert(&store, "exec-1", "claude-opus-5", None);
    insert(&store, "exec-2", "unverified", Some("claude"));

    let aggregates = store
        .get_audit_tool_call_counts_by_attribution(Some(&since()))
        .expect("attribution aggregate");

    assert_eq!(aggregates.len(), 2, "{aggregates:?}");
    assert_eq!(
        bucket(&aggregates, AuditAttribution::Authenticated, "claude").total,
        1
    );
    assert_eq!(
        bucket(&aggregates, AuditAttribution::SelfReported, "claude").total,
        1
    );
}

#[test]
fn rows_without_a_claim_stay_anonymous_and_never_inherit_a_neighbour() {
    let store = Store::open_in_memory().expect("open store");
    insert(&store, "exec-1", "unverified", Some("claude-code"));
    insert(&store, "exec-2", "unverified", None);

    let aggregates = store
        .get_audit_tool_call_counts_by_attribution(Some(&since()))
        .expect("attribution aggregate");

    assert_eq!(
        bucket(&aggregates, AuditAttribution::Anonymous, "anonymous").total,
        1
    );
    assert_eq!(
        bucket(&aggregates, AuditAttribution::SelfReported, "claude-code").total,
        1
    );
}

#[test]
fn a_row_written_before_the_column_existed_reads_as_anonymous() {
    // Simulates every pre-ORB-10890 `unverified` row: the migration adds the
    // column without a backfill, so the value is NULL rather than derived.
    let store = Store::open_in_memory().expect("open store");
    store
        .insert_audit_event_record(&tool_call_params("exec-legacy", "unverified"))
        .expect("insert legacy-shaped row");

    let events = store
        .list_audit_events(&AuditEventFilter::default())
        .expect("list audit events");
    assert_eq!(events[0].self_reported_actor, None);

    let aggregates = store
        .get_audit_tool_call_counts_by_attribution(Some(&since()))
        .expect("attribution aggregate");
    assert_eq!(aggregates.len(), 1, "{aggregates:?}");
    assert_eq!(aggregates[0].attribution, AuditAttribution::Anonymous);
    assert_eq!(aggregates[0].actor, "anonymous");
}

#[test]
fn attribution_split_carries_failure_and_surface_counts() {
    let store = Store::open_in_memory().expect("open store");
    for (index, (status, subcommand)) in [
        (AuditEventStatus::Success, "run-mcp"),
        (AuditEventStatus::Failure, "run-mcp"),
        (AuditEventStatus::Denied, "run"),
    ]
    .iter()
    .enumerate()
    {
        let mut params = tool_call_params(&format!("exec-{index}"), "unverified");
        params.status = *status;
        params.subcommand = Some((*subcommand).to_string());
        store
            .insert_audit_event_record_with_invocation(
                &params,
                AuditInvocationFields {
                    self_reported_actor: Some("claude-code"),
                    ..AuditInvocationFields::default()
                },
            )
            .expect("insert audit event");
    }

    let aggregates = store
        .get_audit_tool_call_counts_by_attribution(Some(&since()))
        .expect("attribution aggregate");

    let row = bucket(&aggregates, AuditAttribution::SelfReported, "claude-code");
    assert_eq!(row.total, 3);
    // `failed` counts both failure and denied, matching the role aggregate.
    assert_eq!(row.failed, 2);
    assert_eq!(row.mcp, 2);
    assert_eq!(row.cli, 1);
}

#[test]
fn events_outside_the_window_are_excluded() {
    let store = Store::open_in_memory().expect("open store");
    insert(&store, "exec-1", "unverified", Some("claude-code"));

    let aggregates = store
        .get_audit_tool_call_counts_by_attribution(Some(&(Utc::now() + Duration::hours(1))))
        .expect("attribution aggregate");

    assert!(aggregates.is_empty(), "{aggregates:?}");
}
