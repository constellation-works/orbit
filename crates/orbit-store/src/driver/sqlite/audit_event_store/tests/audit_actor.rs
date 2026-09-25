//! Canonical-actor aggregation over audit events (ORB-10888).

use crate::AuditEventFilter;
use crate::AuditEventInsertParams;
use crate::Store;
use chrono::{Duration, Utc};
use orbit_types::telemetry::ACTOR_ALIAS_MAP_VERSION;
use orbit_types::telemetry::AuditEventStatus;
use std::collections::BTreeSet;

fn params_with_role(execution_id: &str, role: &str) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: execution_id.to_string(),
        command: "tool".to_string(),
        subcommand: Some("run".to_string()),
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

fn store_with_roles(roles: &[&str]) -> Store {
    let store = Store::open_in_memory().expect("open store");
    for (index, role) in roles.iter().enumerate() {
        store
            .insert_audit_event_record(&params_with_role(&format!("exec-{index}"), role))
            .expect("insert audit event");
    }
    store
}

fn since() -> chrono::DateTime<Utc> {
    Utc::now() - Duration::hours(1)
}

#[test]
fn one_agent_recorded_at_three_granularities_aggregates_as_one_actor() {
    // The production shape: the same actor logged as a family, a shorthand,
    // and a full model string.
    let store = store_with_roles(&["claude", "opus", "claude-opus-5", "claude-opus-5"]);

    let aggregates = store
        .get_audit_event_aggregates_by_actor(&since())
        .expect("aggregate by actor");

    assert_eq!(aggregates.len(), 1, "{aggregates:?}");
    let claude = &aggregates[0];
    assert_eq!(claude.actor, "claude");
    assert_eq!(claude.kind, "agent");
    assert_eq!(claude.family.as_deref(), Some("claude"));
    assert_eq!(claude.vendor.as_deref(), Some("anthropic"));
    assert_eq!(claude.total, 4);
    assert_eq!(claude.cli, 4);
    assert_eq!(claude.mcp, 0);

    // The raw role-grouped aggregate still splits them: this is the defect
    // being fixed, and keeping it observable proves the two views differ.
    let by_role = store
        .get_audit_event_aggregates_by_role(&since())
        .expect("aggregate by role");
    assert_eq!(by_role.len(), 3, "{by_role:?}");
}

#[test]
fn synthetic_and_human_actors_are_separable_by_kind() {
    let store = store_with_roles(&[
        "admin",
        "admin",
        "hook",
        "human",
        "unknown",
        "unverified",
        "claude-opus-5",
    ]);

    let aggregates = store
        .get_audit_event_aggregates_by_actor(&since())
        .expect("aggregate by actor");

    let agents: Vec<_> = aggregates.iter().filter(|a| a.kind == "agent").collect();
    assert_eq!(agents.len(), 1, "{aggregates:?}");
    assert_eq!(agents[0].actor, "claude");

    let kind_of = |actor: &str| {
        aggregates
            .iter()
            .find(|a| a.actor == actor)
            .map(|a| a.kind.clone())
            .unwrap_or_else(|| panic!("missing actor {actor} in {aggregates:?}"))
    };
    assert_eq!(kind_of("admin"), "system");
    assert_eq!(kind_of("hook"), "hook");
    assert_eq!(kind_of("human"), "human");
    assert_eq!(kind_of("unknown"), "unattributed");
    assert_eq!(kind_of("unverified"), "unattributed");

    // `admin` is the largest raw bucket; it must not be able to outrank a real
    // agent in an agents-only view.
    let agent_total: i64 = aggregates
        .iter()
        .filter(|a| a.kind == "agent")
        .map(|a| a.total)
        .sum();
    assert_eq!(agent_total, 1);
}

#[test]
fn actor_projection_is_written_on_insert_without_touching_role() {
    let store = store_with_roles(&["Claude-Opus-5"]);
    let conn = store.read().expect("read connection");

    let (role, kind, id, vendor, family, model, version): (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        u32,
    ) = conn
        .query_row(
            "SELECT role, actor_kind, actor_id, actor_vendor, actor_family, actor_model, \
             actor_alias_version FROM audit_events",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .expect("read actor projection");

    // Byte-for-byte: normalization never rewrites the recorded label.
    assert_eq!(role, "Claude-Opus-5");
    assert_eq!(kind, "agent");
    assert_eq!(id, "claude");
    assert_eq!(vendor.as_deref(), Some("anthropic"));
    assert_eq!(family.as_deref(), Some("claude"));
    assert_eq!(model.as_deref(), Some("Claude-Opus-5"));
    assert_eq!(version, ACTOR_ALIAS_MAP_VERSION);
}

#[test]
fn actor_aggregate_carries_the_same_surface_split_as_the_role_aggregate() {
    let store = Store::open_in_memory().expect("open store");
    for (index, subcommand) in ["run", "run-mcp", "run-mcp", "show"].iter().enumerate() {
        let mut params = params_with_role(&format!("exec-{index}"), "claude-opus-5");
        params.subcommand = Some((*subcommand).to_string());
        store.insert_audit_event_record(&params).expect("insert");
    }

    let aggregates = store
        .get_audit_event_aggregates_by_actor(&since())
        .expect("aggregate by actor");

    assert_eq!(aggregates.len(), 1);
    // `show` counts toward the total but toward neither surface, matching
    // `get_audit_event_aggregates_by_role`.
    assert_eq!(aggregates[0].total, 4);
    assert_eq!(aggregates[0].mcp, 2);
    assert_eq!(aggregates[0].cli, 1);
}

#[test]
fn events_outside_the_window_are_excluded() {
    let store = store_with_roles(&["claude-opus-5"]);
    let future = Utc::now() + Duration::hours(1);

    let aggregates = store
        .get_audit_event_aggregates_by_actor(&future)
        .expect("aggregate by actor");

    assert!(aggregates.is_empty(), "{aggregates:?}");
}

#[test]
fn derived_event_actor_matches_the_persisted_projection() {
    let store = store_with_roles(&["opus", "admin", "unverified", "gpt-6-luna"]);

    let events = store
        .list_audit_events(&AuditEventFilter {
            limit: 100,
            ..Default::default()
        })
        .expect("list audit events");
    assert_eq!(events.len(), 4);

    let conn = store.read().expect("read connection");
    for event in &events {
        let actor = event.actor();
        let (kind, id): (String, String) = conn
            .query_row(
                "SELECT actor_kind, actor_id FROM audit_events WHERE execution_id = ?1",
                [&event.execution_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read persisted projection");
        assert_eq!(actor.kind.to_string(), kind, "{}", event.role);
        assert_eq!(actor.id, id, "{}", event.role);
    }
}
