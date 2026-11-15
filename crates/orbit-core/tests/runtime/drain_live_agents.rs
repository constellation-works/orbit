//! The drain card's live-agent figures count alive provider processes on
//! running and retrying runs, by the activity that started them.

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_store::contracts::V2AuditEventInsertParams;
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use serde_json::{Value, json};

#[test]
fn readiness_counts_live_agents_by_activity() {
    let temp = tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    write_workspace_config(
        &root,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_live_agents".into(),
        },
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&root, &root).unwrap();

    let idle = runtime
        .workspace_auto_readiness(&[], Some(1), 1, &[])
        .unwrap();
    assert_eq!(
        idle["live_agents"],
        json!({"pilots": 0, "recovery": 0, "reviewers": 0}),
        "an idle workspace still reports all three figures at zero"
    );

    let alive = u64::from(std::process::id());
    let dead = 4_000_000_001_u64;
    // Running leaf: two pilots, one recovery, one reviewer, plus an
    // implementer and a finished reviewer that must not count.
    let running = start_run(&runtime, "task_pr_pipeline");
    spawn(&runtime, &running, "pilot-a", "pilot", alive, false);
    spawn(&runtime, &running, "pilot-b", "triager", alive, false);
    spawn(
        &runtime,
        &running,
        "recover-a",
        "step_failure_recovery",
        alive,
        false,
    );
    spawn(&runtime, &running, "review-a", "review", alive, false);
    spawn(
        &runtime,
        &running,
        "implement",
        "implement_one",
        alive,
        false,
    );
    spawn(&runtime, &running, "review-done", "re_review", alive, true);
    // A second running run, so the figures sum across runs. `recover` is the
    // blocked-recovery step id; `final_recovery` is the job-level activity.
    let other = start_run(&runtime, "blocked_task_recovery_pipeline");
    spawn(&runtime, &other, "pilot-c", "repair_pilot", alive, false);
    spawn(
        &runtime,
        &other,
        "recover-b",
        "final_recovery",
        alive,
        false,
    );
    spawn(&runtime, &other, "recover-c", "recover", alive, false);
    spawn(&runtime, &other, "review-b", "landing_review", alive, false);
    spawn(
        &runtime,
        &other,
        "recover-dead",
        "pr_conflict_recovery",
        dead,
        false,
    );
    // Queued work is not live, even when its recorded process is this one.
    let pending = insert_run(&runtime, "task_pilot_pipeline");
    spawn(
        &runtime,
        &pending,
        "pilot-queued",
        "task_pilot",
        alive,
        false,
    );

    let payload = runtime
        .workspace_auto_readiness(&[], Some(1), 1, &[])
        .unwrap();
    assert_eq!(
        payload["live_agents"],
        json!({"pilots": 3, "recovery": 3, "reviewers": 2}),
        "pilots are pilot, repair_pilot, triager and task_pilot; recovery is \
         step_failure_recovery, pr_conflict_recovery, final_recovery and recover; \
         reviewers are review, landing_review, re_review and re_review_2. \
         implement_one, a finished re_review, an exited pr_conflict_recovery \
         and a pending task_pilot do not count"
    );
    assert!(
        payload["capacity"]["running_admitted_workers"].is_number(),
        "the admitted-worker field the header still reads stays on the payload"
    );
}

fn insert_run(runtime: &OrbitRuntime, job_id: &str) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    jobs.insert_job_run(job_id, 1, Utc::now(), None, None)
        .unwrap()
        .run_id
}

fn start_run(runtime: &OrbitRuntime, job_id: &str) -> String {
    let run_id = insert_run(runtime, job_id);
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    jobs.mark_job_run_running(&run_id, Utc::now(), std::process::id())
        .unwrap();
    run_id
}

fn spawn(
    runtime: &OrbitRuntime,
    run_id: &str,
    label: &str,
    activity_name: &str,
    pid: u64,
    finished: bool,
) {
    let activity_id = format!("{run_id}-{label}-activity");
    let process_id = format!("{run_id}-{label}-process");
    insert(
        runtime,
        run_id,
        &activity_id,
        json!({
            "body_kind": "activity_started",
            "activity_name": activity_name,
        }),
    );
    insert(
        runtime,
        run_id,
        &process_id,
        json!({
            "body_kind": "cli_invocation_process",
            "parent_event_id": activity_id,
            "provider": "codex",
            "pid": pid,
        }),
    );
    if finished {
        insert(
            runtime,
            run_id,
            &format!("{run_id}-{label}-finished"),
            json!({
                "body_kind": "cli_invocation_finished",
                "parent_event_id": activity_id,
                "provider": "codex",
                "exit_code": 0,
            }),
        );
    }
}

fn insert(runtime: &OrbitRuntime, run_id: &str, event_id: &str, body: Value) {
    let mut payload = body;
    payload["event_id"] = json!(event_id);
    payload["run_id"] = json!(run_id);
    payload["ts"] = json!(Utc::now().to_rfc3339());
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().unwrap(),
            event_id: event_id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: payload["body_kind"]
                .as_str()
                .unwrap_or("activity_started")
                .into(),
            ts: Utc::now(),
            run_id: run_id.into(),
            agent_identity: "codex".into(),
            parent_event_id: payload["parent_event_id"].as_str().map(str::to_string),
            workspace_path: None,
            payload_json: payload.to_string(),
        })
        .unwrap();
}
