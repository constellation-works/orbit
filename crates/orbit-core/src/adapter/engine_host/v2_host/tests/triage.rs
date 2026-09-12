//! Tests for the triage deterministic actions [ORB-10129]: candidate
//! listing guards (human-blocked tasks are untouchable), the environmental
//! re-backlog path end-to-end from a failed `task_pr_pipeline` run, the
//! stay-blocked diagnosis path, the durable re-backlog loop guard, and
//! idempotency under replay/overlap.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT};
use orbit_store::friction_store::FrictionListFilter;
use orbit_store::{JobRunStepParams, TaskCreateParams, TaskReservationReleaseReason};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRunState, JobTargetType};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::super::triage::*;
use crate::OrbitRuntime;

const PIPELINE_JOB: &str = "task_pr_pipeline";
const ENVIRONMENTAL_STEP_MESSAGE: &str =
    "cli subprocess reported envelope status=\"failed\" despite exit 0";

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

fn create_backlog_task(
    runtime: &OrbitRuntime,
    _repo_root: &std::path::Path,
    id_hint: &str,
) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: format!("task {id_hint}"),
            description: "test".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "test plan".to_string(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
        .id
}

/// Drive one full ORB-10127 failure cycle: insert a running
/// `task_pr_pipeline` run, couple the task to it, record an
/// environmental-looking failing step, and finalize the run as failed. The
/// coupling-out hook moves the task to `blocked`. Returns the run id.
fn fail_pipeline_run_for_task(runtime: &OrbitRuntime, task_id: &str) -> String {
    fail_pipeline_attempt(runtime, task_id, None, std::process::id())
}

fn fail_pipeline_attempt(
    runtime: &OrbitRuntime,
    task_id: &str,
    retry: Option<String>,
    pid: u32,
) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(PIPELINE_JOB, 1, Utc::now(), None, retry)
        .expect("insert pipeline run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), pid)
        .expect("mark run running");
    runtime
        .apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("couple task to run");
    let now = Utc::now();
    runtime
        .stores()
        .jobs()
        .complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "agent_implement".to_string(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".to_string()),
                error_message: Some(ENVIRONMENTAL_STEP_MESSAGE.to_string()),
            },
        )
        .expect("record failing step");
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Failed,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize failed run");
    assert_eq!(
        runtime.get_task(task_id).expect("task").status,
        TaskStatus::Blocked,
        "ORB-10127 coupling must block the task before triage can see it"
    );
    run.run_id
}

fn list_candidates(runtime: &OrbitRuntime, input: Value) -> Value {
    list_triage_candidates(runtime, "list_triage_candidates", &input)
        .expect("list triage candidates")
}

fn apply_dispositions(runtime: &OrbitRuntime, input: Value) -> Value {
    apply_triage_dispositions(runtime, "apply_triage_dispositions", &input)
        .expect("apply triage dispositions")
}

fn environmental_disposition(task_id: &str) -> Value {
    json!({
        "task_id": task_id,
        "classification": "environmental",
        "disposition": "rebacklog",
        "diagnosis": "provider auth failure on the box, not the task's fault",
        "mitigation": "released stale worktree",
    })
}

fn history_events(runtime: &OrbitRuntime, task_id: &str, event: &str) -> usize {
    runtime
        .get_task_history(task_id)
        .expect("task history")
        .into_iter()
        .filter(|entry| entry.event == event)
        .count()
}

fn friction_count(runtime: &OrbitRuntime) -> usize {
    crate::runtime::friction::store_for(runtime)
        .expect("friction store")
        .list(&FrictionListFilter::default())
        .expect("list frictions")
        .len()
}

#[test]
fn human_blocked_tasks_are_never_candidates() {
    let (_root, runtime, repo_root) = test_runtime();

    // Blocked by hand: no coupled run at all.
    let hand_blocked = create_backlog_task(&runtime, &repo_root, "hand-blocked");
    runtime
        .apply_task_automation_update(
            &hand_blocked,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("block by hand");

    // Blocked by hand after its run succeeded: coupled run is non-failed.
    let succeeded_run_task = create_backlog_task(&runtime, &repo_root, "succeeded-run");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(PIPELINE_JOB, 1, Utc::now(), None, None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark running");
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize success run");
    runtime
        .apply_task_automation_update(
            &succeeded_run_task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                job_run_id: Some(run.run_id.clone()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("block by hand with succeeded run");

    let output = list_candidates(&runtime, json!({}));
    assert_eq!(output["candidate_count"], json!(0));
    assert_eq!(output["candidates"], json!([]));

    // Even a forged disposition cannot touch them: they are not candidates.
    let applied = apply_dispositions(
        &runtime,
        json!({
            "dispositions": [environmental_disposition(&hand_blocked)],
            "candidates": [],
        }),
    );
    assert_eq!(applied["rebacklogged_count"], json!(0));
    assert_eq!(applied["skipped_count"], json!(1));
    assert_eq!(
        runtime.get_task(&hand_blocked).expect("task").status,
        TaskStatus::Blocked,
        "human intent wins: triage must never move a hand-blocked task"
    );
}

/// End-to-end deterministic trail (AC 11): failed `task_pr_pipeline` run →
/// ORB-10127 blocks the task → triage lists it → environmental disposition
/// returns it to backlog with `workflow_run_failed` → `triage_rebacklogged`
/// in its history.
#[test]
fn environmental_failure_returns_task_to_backlog_with_history_trail() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "environmental");
    let run_id = fail_pipeline_run_for_task(&runtime, &task_id);

    let output = list_candidates(&runtime, json!({}));
    assert_eq!(output["candidate_count"], json!(1));
    let candidate = &output["candidates"][0];
    assert_eq!(candidate["task_id"], json!(task_id));
    assert_eq!(candidate["run_id"], json!(run_id));
    assert_eq!(candidate["job_id"], json!(PIPELINE_JOB));
    assert_eq!(
        candidate["error_message"],
        json!(ENVIRONMENTAL_STEP_MESSAGE),
        "the agent's evidence must carry the failing step's error"
    );
    assert_eq!(candidate["rebacklog_count"], json!(0));

    let applied = apply_dispositions(
        &runtime,
        json!({
            "dispositions": [environmental_disposition(&task_id)],
            "candidates": output["candidates"],
        }),
    );
    assert_eq!(applied["rebacklogged_count"], json!(1));

    let task = runtime.get_task(&task_id).expect("task after triage");
    assert_eq!(task.status, TaskStatus::Backlog);

    let history = runtime.get_task_history(&task_id).expect("history");
    let failed_index = history
        .iter()
        .position(|entry| entry.event == WORKFLOW_RUN_FAILED_EVENT)
        .expect("workflow_run_failed event recorded");
    let triage_index = history
        .iter()
        .position(|entry| entry.event == TRIAGE_REBACKLOGGED_EVENT)
        .expect("triage_rebacklogged event recorded");
    assert!(
        failed_index < triage_index,
        "history trail must read workflow_run_failed → triage re-backlog"
    );
    let entry = &history[triage_index];
    assert_eq!(entry.to_status, Some(TaskStatus::Backlog));
    let note = entry.note.as_deref().unwrap_or_default();
    assert!(note.contains(&run_id), "note names the failed run: {note}");
    assert!(
        note.contains("environmental"),
        "note names the classification: {note}"
    );
    assert!(
        note.contains("released stale worktree"),
        "note names the mitigation taken: {note}"
    );
}

#[test]
fn non_environmental_diagnoses_stay_blocked_and_deny_rebacklog() {
    let (_root, runtime, repo_root) = test_runtime();
    let code_defect = create_backlog_task(&runtime, &repo_root, "code-defect");
    let smuggled = create_backlog_task(&runtime, &repo_root, "smuggled");
    fail_pipeline_run_for_task(&runtime, &code_defect);
    fail_pipeline_run_for_task(&runtime, &smuggled);

    let output = list_candidates(&runtime, json!({}));
    assert_eq!(output["candidate_count"], json!(2));

    let applied = apply_dispositions(
        &runtime,
        json!({
            "dispositions": [
                {
                    "task_id": code_defect,
                    "classification": "code_defect",
                    "disposition": "stay_blocked",
                    "diagnosis": "tests are genuinely red on this branch",
                },
                {
                    // A non-environmental classification cannot buy a
                    // re-backlog, no matter what the agent requests.
                    "task_id": smuggled,
                    "classification": "task_defect",
                    "disposition": "rebacklog",
                    "diagnosis": "context files look stale",
                },
            ],
            "candidates": output["candidates"],
        }),
    );
    assert_eq!(applied["diagnosed_count"], json!(2));
    assert_eq!(applied["rebacklogged_count"], json!(0));

    for task_id in [&code_defect, &smuggled] {
        let task = runtime.get_task(task_id).expect("task");
        assert_eq!(task.status, TaskStatus::Blocked, "{task_id} stays blocked");
        assert_eq!(history_events(&runtime, task_id, TRIAGE_DIAGNOSIS_EVENT), 1);
    }
    let history = runtime.get_task_history(&smuggled).expect("history");
    let note = history
        .iter()
        .rev()
        .find(|entry| entry.event == TRIAGE_DIAGNOSIS_EVENT)
        .and_then(|entry| entry.note.clone())
        .unwrap_or_default();
    assert!(
        note.contains("re-backlog denied"),
        "denied re-backlog must be visible in the diagnosis note: {note}"
    );
}

#[test]
fn diagnosed_run_is_suppressed_until_new_failure_or_explicit_request() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "repeat-diagnosis");
    let first_run_id = fail_pipeline_run_for_task(&runtime, &task_id);

    let first_listing = list_candidates(&runtime, json!({}));
    let diagnosed = apply_dispositions(
        &runtime,
        json!({
            "dispositions": [{
                "task_id": task_id,
                "classification": "code_defect",
                "disposition": "stay_blocked",
                "diagnosis": "tests are genuinely red on this branch",
            }],
            "candidates": first_listing["candidates"],
        }),
    );
    assert_eq!(diagnosed["diagnosed_count"], json!(1));

    let repeated = list_candidates(&runtime, json!({}));
    assert_eq!(
        repeated["candidate_count"],
        json!(0),
        "the same coupled failed run must await human action"
    );

    let forced = list_candidates(&runtime, json!({ "task_ids": [task_id] }));
    assert_eq!(forced["candidate_count"], json!(1));
    assert_eq!(forced["candidates"][0]["run_id"], json!(first_run_id));

    let second_run_id = fail_pipeline_run_for_task(&runtime, &task_id);
    assert_ne!(second_run_id, first_run_id);
    let after_new_failure = list_candidates(&runtime, json!({}));
    assert_eq!(after_new_failure["candidate_count"], json!(1));
    assert_eq!(
        after_new_failure["candidates"][0]["run_id"],
        json!(second_run_id),
        "a diagnosis for the old run must not suppress new failure evidence"
    );
}

#[test]
fn landed_work_stays_blocked_with_diagnosis_for_human_reconciliation() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "landed-externally");
    fail_pipeline_run_for_task(&runtime, &task_id);
    let listing = list_candidates(&runtime, json!({}));

    let applied = apply_dispositions(
        &runtime,
        json!({
            "dispositions": [{
                "task_id": task_id,
                "classification": "unknown",
                "disposition": "stay_blocked",
                "diagnosis": "work already landed: PR #619 merged as abc123 on agent-main",
            }],
            "candidates": listing["candidates"],
        }),
    );
    assert_eq!(applied["diagnosed_count"], json!(1));
    assert_eq!(applied["skipped_count"], json!(0));
    assert_eq!(
        runtime.get_task(&task_id).expect("task after apply").status,
        TaskStatus::Blocked,
        "landed work awaits lifecycle-legal human reconciliation"
    );
    let history = runtime.get_task_history(&task_id).expect("history");
    let note = history
        .iter()
        .rev()
        .find(|entry| entry.event == TRIAGE_DIAGNOSIS_EVENT)
        .and_then(|entry| entry.note.as_deref())
        .expect("triage diagnosis recorded");
    assert!(
        note.contains("PR #619 merged as abc123 on agent-main"),
        "the durable diagnosis must preserve the exact landing evidence: {note}"
    );
}

/// Loop guard (AC 5): drive blocked → backlog → blocked to exhaustion with
/// `max_rebacklogs: 2`. The third failure is not re-backlogged: the task
/// stays blocked with a `triage_gave_up` note, a friction is filed, and
/// repeated triage passes change nothing further.
#[test]
fn loop_guard_gives_up_after_budget_and_files_friction_once() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "ping-pong");

    for cycle in 0..2 {
        fail_pipeline_run_for_task(&runtime, &task_id);
        let output = list_candidates(&runtime, json!({ "max_rebacklogs": 2 }));
        assert_eq!(
            output["candidate_count"],
            json!(1),
            "cycle {cycle}: task within budget must be a candidate"
        );
        assert_eq!(output["candidates"][0]["rebacklog_count"], json!(cycle));
        let applied = apply_dispositions(
            &runtime,
            json!({
                "dispositions": [environmental_disposition(&task_id)],
                "candidates": output["candidates"],
                "max_rebacklogs": 2,
            }),
        );
        assert_eq!(applied["rebacklogged_count"], json!(1), "cycle {cycle}");
        assert_eq!(
            runtime.get_task(&task_id).expect("task").status,
            TaskStatus::Backlog
        );
    }

    // Third failure, same way: budget exhausted.
    fail_pipeline_run_for_task(&runtime, &task_id);
    let output = list_candidates(&runtime, json!({ "max_rebacklogs": 2 }));
    assert_eq!(
        output["candidate_count"],
        json!(0),
        "exhausted task must not be offered to the agent again"
    );
    assert_eq!(output["exhausted"][0]["task_id"], json!(task_id));
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );
    assert_eq!(history_events(&runtime, &task_id, TRIAGE_GAVE_UP_EVENT), 1);
    assert_eq!(friction_count(&runtime), 1, "gave-up files one friction");

    // Even a direct re-backlog request cannot revive it.
    let applied = apply_dispositions(
        &runtime,
        json!({
            "dispositions": [environmental_disposition(&task_id)],
            "candidates": [{ "task_id": task_id, "run_id": runtime
                .get_task(&task_id)
                .expect("task")
                .job_run_id
                .expect("coupled run") }],
            "max_rebacklogs": 2,
        }),
    );
    assert_eq!(applied["gave_up_count"], json!(1));
    assert_eq!(applied["rebacklogged_count"], json!(0));
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );
    assert_eq!(
        history_events(&runtime, &task_id, TRIAGE_REBACKLOGGED_EVENT),
        2,
        "no third re-backlog, ever"
    );

    // Gave-up handling is idempotent across repeated passes: no duplicate
    // notes, no duplicate frictions.
    let _ = list_candidates(&runtime, json!({ "max_rebacklogs": 2 }));
    assert_eq!(history_events(&runtime, &task_id, TRIAGE_GAVE_UP_EVENT), 1);
    assert_eq!(friction_count(&runtime), 1);
}

/// Overlap/idempotency (AC 7): replaying the same disposition set (two
/// overlapping triage runs racing) cannot double-transition the task or
/// duplicate history — the second write sees a non-blocked task and skips.
#[test]
fn replayed_dispositions_do_not_double_transition() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "replay");
    fail_pipeline_run_for_task(&runtime, &task_id);

    let output = list_candidates(&runtime, json!({}));
    let payload = json!({
        // Duplicate entry inside one batch AND a full replay of the batch.
        "dispositions": [
            environmental_disposition(&task_id),
            environmental_disposition(&task_id),
        ],
        "candidates": output["candidates"],
    });

    let first = apply_dispositions(&runtime, payload.clone());
    assert_eq!(first["rebacklogged_count"], json!(1));
    assert_eq!(first["skipped_count"], json!(1), "in-batch duplicate skips");

    let second = apply_dispositions(&runtime, payload);
    assert_eq!(second["rebacklogged_count"], json!(0));
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Backlog
    );
    assert_eq!(
        history_events(&runtime, &task_id, TRIAGE_REBACKLOGGED_EVENT),
        1,
        "replay must not duplicate the transition or its history"
    );
}

#[test]
fn empty_candidate_set_is_a_clean_no_op() {
    let (_root, runtime, _repo_root) = test_runtime();
    let output = list_candidates(&runtime, json!({}));
    assert_eq!(output["candidate_count"], json!(0));
    assert_eq!(output["candidates"], json!([]));
    assert_eq!(output["task_ids"], json!([]));

    let applied = apply_dispositions(&runtime, json!({ "dispositions": [], "candidates": [] }));
    assert_eq!(applied["rebacklogged_count"], json!(0));
    assert_eq!(applied["skipped_count"], json!(0));
}

#[test]
fn explicit_task_ids_narrow_the_scan_but_keep_the_guards() {
    let (_root, runtime, repo_root) = test_runtime();
    let listed = create_backlog_task(&runtime, &repo_root, "listed");
    let other = create_backlog_task(&runtime, &repo_root, "other");
    fail_pipeline_run_for_task(&runtime, &listed);
    fail_pipeline_run_for_task(&runtime, &other);
    let hand_blocked = create_backlog_task(&runtime, &repo_root, "hand");
    runtime
        .apply_task_automation_update(
            &hand_blocked,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("block by hand");

    let output = list_candidates(&runtime, json!({ "task_ids": [listed, hand_blocked] }));
    assert_eq!(output["candidate_count"], json!(1));
    assert_eq!(output["candidates"][0]["task_id"], json!(listed));
}

#[test]
fn later_human_block_with_same_run_id_is_not_rebacklogged() {
    let (_root, runtime, repo) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo, "human-intent");
    fail_pipeline_run_for_task(&runtime, &task_id);
    let prepared = list_candidates(&runtime, json!({}));
    runtime
        .update_task(
            &task_id,
            crate::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Blocked),
                comment: Some("Wait for a product decision".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let applied = apply_dispositions(
        &runtime,
        json!({
            "candidates": prepared["candidates"], "dispositions": [environmental_disposition(&task_id)]
        }),
    );
    assert_eq!(applied["rebacklogged_count"], 0);
    assert_eq!(
        runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Blocked
    );
}

#[test]
#[cfg(unix)]
fn state_incident_waits_for_exact_retry_lineage_and_retains_episode_identity() {
    use orbit_automation::consumers::incidents::observe;
    let (_root, runtime, repo) = test_runtime();
    let task = create_backlog_task(&runtime, &repo, "state-retry");
    let first = fail_pipeline_attempt(&runtime, &task, None, i32::MAX as u32);
    let original = observe(&runtime, &runtime.get_task(&task).unwrap()).unwrap();
    let retry = runtime
        .stores()
        .jobs()
        .insert_job_run(PIPELINE_JOB, 2, Utc::now(), None, Some(first.clone()))
        .unwrap();
    assert!(observe(&runtime, &runtime.get_task(&task).unwrap()).is_err());
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&retry.run_id, Utc::now(), i32::MAX as u32)
        .unwrap();
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &retry.run_id,
            JobRunState::Failed,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .unwrap();
    assert_eq!(
        original.0,
        observe(&runtime, &runtime.get_task(&task).unwrap())
            .unwrap()
            .0
    );
    let last = fail_pipeline_attempt(&runtime, &task, Some(first.clone()), i32::MAX as u32);
    let retried = observe(&runtime, &runtime.get_task(&task).unwrap()).unwrap();
    assert_eq!(original.0, retried.0);
    assert_eq!(retried.1["episode"], first);
    assert_eq!(retried.1["cause_run_id"], last);
    // Unrelated in-flight work does not make this causal episode unsettled.
    runtime
        .stores()
        .jobs()
        .insert_job_run(PIPELINE_JOB, 1, Utc::now(), None, None)
        .unwrap();
    assert!(observe(&runtime, &runtime.get_task(&task).unwrap()).is_ok());
}

#[test]
#[cfg(unix)]
fn state_incident_collapses_parent_child_and_withholds_live_parent() {
    use orbit_automation::consumers::incidents::observe;
    use orbit_types::workflow::{ChildDispatch, ChildDispatchPhase, PipelineState};
    let (_root, runtime, repo) = test_runtime();
    let child_task = create_backlog_task(&runtime, &repo, "child");
    let child = fail_pipeline_attempt(&runtime, &child_task, None, i32::MAX as u32);
    let expected = observe(&runtime, &runtime.get_task(&child_task).unwrap())
        .unwrap()
        .0;
    let mut expected_members = vec![child_task.clone()];
    for (hint, pid, allowed) in [
        ("stopped-parent", i32::MAX as u32, true),
        ("live-parent", std::process::id(), false),
    ] {
        let parent_task = create_backlog_task(&runtime, &repo, hint);
        let parent = fail_pipeline_attempt(&runtime, &parent_task, None, pid);
        let mut state = PipelineState::new(parent.clone(), PIPELINE_JOB.into(), json!({}));
        let mut dispatch = ChildDispatch::submitted(
            child.clone(),
            PIPELINE_JOB.into(),
            "invoke".into(),
            true,
            false,
            Utc::now(),
        );
        dispatch.phase = ChildDispatchPhase::Terminal;
        dispatch.child_status = Some("failed".into());
        state.record_child_dispatch(dispatch);
        runtime.write_run_state(&parent, &state).unwrap();
        let observed = observe(&runtime, &runtime.get_task(&parent_task).unwrap());
        if allowed {
            assert_eq!(observed.unwrap().0, expected);
            expected_members.push(parent_task);
            expected_members.sort();
            assert_eq!(
                orbit_automation::consumers::incidents::members(&runtime).unwrap()[&expected],
                expected_members
            );
        } else {
            assert!(observed.is_err());
        }
    }
    assert!(
        !orbit_automation::consumers::incidents::members(&runtime)
            .unwrap()
            .contains_key(&expected)
    );
}
