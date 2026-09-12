//! Runtime host-store reuse: accessors and dispatch clone the builder-opened
//! audit database instead of calling `Store::open` again.

use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use orbit_store::Store;
use orbit_store::contracts::{
    GrantInsertOutcome, RecoveryBudget, RecoveryReserveRequest, ReviewReserveRequest,
    V2AuditEventFilter, V2AuditEventInsertParams,
};
use orbit_types::workflow::automation::{AutomationState, SourceRevision};
use orbit_types::workflow::{
    GrantLimits, GrantRights, GrantStatus, OperationGrant, RecoveryEpisodeKind, ReviewBudget,
    ReviewReservation,
};
use serde_json::json;

use crate::OrbitRuntime;
use crate::adapter::command::ToolEntryPoint;

const ACCESS_ITERS: u32 = 32;
const DISPATCH_ITERS: u32 = 16;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("in-memory runtime")
}

fn audit_db(runtime: &OrbitRuntime) -> std::path::PathBuf {
    runtime.context.persistence().audit_db.clone()
}

fn revision(label: &str) -> SourceRevision {
    SourceRevision {
        commit: format!("commit-{label}"),
        tree: format!("tree-{label}"),
    }
}

fn automation_state(consumer: &str) -> AutomationState {
    let head = revision("head");
    AutomationState {
        members: None,
        consumer: consumer.into(),
        epoch: "epoch".into(),
        trigger: None,
        repository: "repo".into(),
        branch: "agent-main".into(),
        generation: 0,
        baseline: head.clone(),
        observed: head.clone(),
        covered: head,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
    }
}

fn grant(workspace_id: &str, id: &str, task_id: &str) -> OperationGrant {
    let now = Utc::now();
    OperationGrant {
        id: id.to_string(),
        workspace_id: workspace_id.to_string(),
        actor: "operator".to_string(),
        source: "test".to_string(),
        created_at: now,
        expires_at: now + ChronoDuration::seconds(3_600),
        revision: 1,
        task_ids: vec![task_id.to_string()],
        rights: GrantRights {
            prepare: true,
            promote: true,
            complete: false,
        },
        limits: GrantLimits {
            leaf_ceiling: 2,
            preparation_due_seconds: 300,
            recovery_episodes_per_task: 2,
            recovery_minutes_per_task: 30,
        },
        policy: json!({ "version": 1 }),
        policy_version: 1,
        status: GrantStatus::Active,
        stopped: None,
        revoked: None,
    }
}

fn touch_host_accessors(runtime: &OrbitRuntime) {
    runtime.automation_store().expect("automation");
    runtime.review_store().expect("review");
    runtime.operation_store().expect("operation");
    runtime.v2_audit_store().expect("v2 audit");
    runtime.sqlite_store().expect("sqlite");
}

fn dispatch_search(runtime: &OrbitRuntime) {
    runtime
        .execute_tool_command_dispatch(
            "orbit.search",
            json!({
                "query": "reuse",
                "model": orbit_common::test_fixtures::TEST_CODEX_MODEL
            }),
            None,
            None,
            ToolEntryPoint::Mcp,
        )
        .expect("dispatch");
}

#[test]
fn repeated_host_access_does_not_reopen_the_audit_database() {
    let runtime = runtime();
    let audit_db = audit_db(&runtime);
    touch_host_accessors(&runtime);
    dispatch_search(&runtime);
    let opened = Store::thread_file_open_count_for(&audit_db);

    for _ in 0..ACCESS_ITERS {
        touch_host_accessors(&runtime);
    }
    for _ in 0..DISPATCH_ITERS {
        dispatch_search(&runtime);
    }

    assert_eq!(
        Store::thread_file_open_count_for(&audit_db),
        opened,
        "repeated accessor and dispatch use must not call Store::open on the runtime audit database"
    );
}

#[test]
fn worker_preflight_still_reopens_the_audit_database() {
    let runtime = runtime();
    let audit_db = audit_db(&runtime);
    let opened = Store::thread_file_open_count_for(&audit_db);
    runtime
        .ensure_persistence_ready()
        .expect("documented worker preflight");
    assert!(
        Store::thread_file_open_count_for(&audit_db) > opened,
        "ensure_persistence_ready must keep its justified reopen"
    );
}

#[test]
fn reused_handles_keep_partitioned_persistence_and_recovery() {
    let runtime = runtime();
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let other_workspace = "ws_other";

    let first_automation = runtime.automation_store().expect("automation");
    let first_review = runtime.review_store().expect("review");
    let first_operation = runtime.operation_store().expect("operation");
    let first_audit = runtime.v2_audit_store().expect("v2 audit");

    let consumer_a = format!("{workspace_id}/consumer-a");
    let consumer_b = format!("{workspace_id}/consumer-b");
    let state_a = automation_state(&consumer_a);
    let state_b = automation_state(&consumer_b);
    assert!(
        first_automation
            .automation_initialize(&state_a)
            .expect("init a")
    );
    assert!(
        first_automation
            .automation_initialize(&state_b)
            .expect("init b")
    );

    let task_ids = vec!["ORB-1".to_string()];
    let ReviewReservation::Reserved { attempt } = first_review
        .review_reserve(
            &workspace_id,
            &ReviewReserveRequest {
                lineage_key: "ws/ORB-1/agent-main",
                task_ids: &task_ids,
                run_id: "jrun-reuse",
                task_meaning_digest: "meaning",
                candidate: &revision("impl"),
                budget: ReviewBudget::default(),
                now: Utc::now(),
            },
        )
        .expect("reserve review")
        .0
    else {
        panic!("first review start reserves");
    };

    assert_eq!(
        first_operation
            .operation_grant_insert(&grant(&workspace_id, "ogrant-1", "ORB-1"))
            .expect("insert grant"),
        GrantInsertOutcome::Inserted
    );
    assert_eq!(
        first_operation
            .operation_grant_insert(&grant(other_workspace, "ogrant-other", "ORB-2"))
            .expect("insert other grant"),
        GrantInsertOutcome::Inserted
    );

    let (reservation, _) = first_operation
        .operation_recovery_reserve(
            &workspace_id,
            &RecoveryReserveRequest {
                task_id: "ORB-1",
                run_id: "jrun-recovery",
                step_id: Some("sync_base"),
                kind: RecoveryEpisodeKind::StepRecovery,
                budget: RecoveryBudget {
                    episodes: 2,
                    seconds: 1_800,
                },
                now: Utc::now(),
            },
        )
        .expect("reserve recovery");
    assert!(matches!(
        reservation,
        orbit_types::workflow::RecoveryReservation::Reserved { episode: 1, .. }
    ));

    first_audit
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: workspace_id.clone(),
            event_id: "evt-reuse".into(),
            source: "test".into(),
            schema_version: 1,
            event_type: "reuse.persisted".into(),
            ts: Utc::now(),
            run_id: "jrun-reuse".into(),
            agent_identity: "grok".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({ "ok": true }).to_string(),
        })
        .expect("insert v2 audit");

    let second_automation = runtime.automation_store().expect("automation reuse");
    let second_review = runtime.review_store().expect("review reuse");
    let second_operation = runtime.operation_store().expect("operation reuse");
    let second_audit = runtime.v2_audit_store().expect("v2 audit reuse");

    assert_eq!(
        second_automation
            .automation_state(&consumer_a)
            .expect("read a"),
        Some(state_a)
    );
    assert_eq!(
        second_automation
            .automation_state(&consumer_b)
            .expect("read b")
            .map(|state| state.consumer),
        Some(consumer_b)
    );
    assert_eq!(
        second_review
            .review_ledger(&workspace_id, "ws/ORB-1/agent-main")
            .expect("ledger")
            .expect("present")
            .attempts[0]
            .attempt_id,
        attempt.attempt_id
    );
    assert!(
        second_review
            .review_ledger(other_workspace, "ws/ORB-1/agent-main")
            .expect("other ledger")
            .is_none(),
        "review ledgers are partitioned by workspace"
    );
    assert_eq!(
        second_operation
            .operation_active_grant(&workspace_id, Utc::now())
            .expect("active grant")
            .map(|grant| grant.id),
        Some("ogrant-1".into())
    );
    assert_eq!(
        second_operation
            .operation_active_grant(other_workspace, Utc::now())
            .expect("other grant")
            .map(|grant| grant.id),
        Some("ogrant-other".into())
    );
    assert_eq!(
        second_operation
            .operation_recovery_ledger(&workspace_id, "ORB-1")
            .expect("recovery ledger")
            .expect("present")
            .episodes_consumed(),
        1
    );
    assert_eq!(
        second_audit
            .list_v2_audit_events(&V2AuditEventFilter {
                workspace_id: workspace_id.clone(),
                event_type: Some("reuse.persisted".into()),
                ..V2AuditEventFilter::default()
            })
            .expect("list audit")
            .len(),
        1
    );
}

#[test]
fn reports_bounded_warm_reuse_versus_reopen_measurements() {
    let runtime = runtime();
    let audit_db = audit_db(&runtime);
    touch_host_accessors(&runtime);
    dispatch_search(&runtime);

    let reused_access = time_iters(ACCESS_ITERS, || touch_host_accessors(&runtime));
    let control_open = time_iters(ACCESS_ITERS, || {
        drop(Store::open(&audit_db).expect("control open"));
    });
    let reused_dispatch = time_iters(DISPATCH_ITERS, || dispatch_search(&runtime));

    assert!(
        reused_access < Duration::from_secs(30)
            && control_open < Duration::from_secs(30)
            && reused_dispatch < Duration::from_secs(30),
        "warm reused accessors {reused_access:?}; same-path Store::open control {control_open:?}; warm dispatch {reused_dispatch:?}"
    );
}

fn time_iters(iters: u32, mut body: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        body();
    }
    start.elapsed()
}
