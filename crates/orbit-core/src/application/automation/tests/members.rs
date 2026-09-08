//! Host observe/admission fixtures for incident inventory reuse [ORB-11633].

use super::super::{
    members::Host,
    preparation,
    source::{ls_tree_invocations, reset_ls_tree_invocations},
};
use crate::OrbitRuntime;
use chrono::Utc;
use orbit_automation::members::{MemberAdmission, MemberHost};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::{JobRunStepParams, TaskCreateParams, TaskReservationReleaseReason};
use orbit_types::{
    task::{TaskPriority, TaskStatus, TaskType},
    workflow::{
        ChildDispatch, ChildDispatchPhase, JobRunState, JobTargetType, PipelineState,
        automation::members::{StateTrigger, StateTriggerKind},
    },
};
use serde_json::json;
use std::{path::Path, process::Command};
use tempfile::tempdir;

const PIPELINE_JOB: &str = "task_pr_pipeline";
const FAILING_STEP: &str = "cli subprocess reported envelope status=\"failed\" despite exit 0";

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    git(&repo_root, &["init", "--initial-branch=agent-main"]);
    git(&repo_root, &["config", "user.name", "Test"]);
    git(
        &repo_root,
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(repo_root.join("sample.txt"), "baseline").unwrap();
    git(&repo_root, &["add", "sample.txt"]);
    git(&repo_root, &["commit", "-m", "baseline"]);
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .expect("build test runtime")
        .with_automation_machine_identity(Some("fixture-machine".into()));
    (root, runtime, repo_root)
}

fn trigger() -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::ExecutionFailed,
        owner_machine: "fixture-machine".into(),
        branch: "agent-main".into(),
        debounce_minutes: 1,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 0,
        deadline_minutes: 30,
    }
}

fn preparation_trigger() -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        ..trigger()
    }
}

fn create_backlog_task(runtime: &OrbitRuntime, repo_root: &Path, id_hint: &str) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".into(),
            parent_id: None,
            title: format!("task {id_hint}"),
            description: "test".into(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "test plan".into(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            workspace_path: Some(repo_root.to_string_lossy().into_owned()),
            repo_root: None,
            created_by: Some("test".into()),
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

fn create_proposed_task(runtime: &OrbitRuntime, repo_root: &Path, id_hint: &str) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".into(),
            parent_id: None,
            title: format!("task {id_hint}"),
            description: "test".into(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "test plan".into(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            workspace_path: Some(repo_root.to_string_lossy().into_owned()),
            repo_root: None,
            created_by: Some("test".into()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Proposed,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create proposed task")
        .id
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
                target_id: "agent_implement".into(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".into()),
                error_message: Some(FAILING_STEP.into()),
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
    run.run_id
}

fn dead_pid() -> u32 {
    i32::MAX as u32
}

fn assert_retire(admission: MemberAdmission, expected: &str) {
    match admission {
        MemberAdmission::Retire(reason) => assert_eq!(reason, expected),
        MemberAdmission::Admit => panic!("admitted instead of retire {expected}"),
        MemberAdmission::Withhold(reason) => {
            panic!("withheld {reason} instead of retire {expected}")
        }
    }
}

fn couple_parent_to_child(
    runtime: &OrbitRuntime,
    parent_task: &str,
    child_run: &str,
    pid: u32,
) -> String {
    let parent = fail_pipeline_attempt(runtime, parent_task, None, pid);
    let mut state = PipelineState::new(parent.clone(), PIPELINE_JOB.into(), json!({}));
    let mut dispatch = ChildDispatch::submitted(
        child_run.into(),
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
    parent
}

#[test]
fn preparation_page_lists_instructions_once_for_all_eligible_tasks() {
    let (_root, runtime, repo) = test_runtime();
    for hint in ["one", "two", "three"] {
        create_proposed_task(&runtime, &repo, hint);
    }

    reset_ls_tree_invocations();
    let trigger = preparation_trigger();
    let page = Host::new(&runtime, &trigger)
        .observe(None, Utc::now())
        .unwrap();

    assert_eq!(page.candidates.len(), 3);
    assert_eq!(
        ls_tree_invocations(),
        1,
        "one revision-scoped instruction listing serves the entire page"
    );
}

#[test]
fn cached_instruction_snapshot_preserves_preparation_fingerprint_bytes() {
    let (_root, runtime, repo) = test_runtime();
    std::fs::write(repo.join("AGENTS.md"), "Follow the pinned instructions.\n")
        .expect("write instructions");
    git(&repo, &["add", "AGENTS.md"]);
    git(&repo, &["commit", "-m", "add instructions"]);

    let id = create_proposed_task(&runtime, &repo, "fingerprint");
    let task = runtime.get_task(&id).expect("task");
    let revision = preparation::head_revision(&runtime, "agent-main").expect("head");

    let before = preparation::fingerprint(&runtime, &task, &revision).expect("fingerprint");
    let instructions = preparation::instructions(&runtime, &revision).expect("instructions");
    let after =
        preparation::fingerprint_with_instructions(&runtime, &task, &revision, &instructions)
            .expect("fingerprint with cached instructions");

    assert_eq!(before, after);
}

#[test]
fn stale_membership_between_observe_and_admission_is_retired() {
    let (_root, runtime, repo) = test_runtime();
    let child_task = create_backlog_task(&runtime, &repo, "child");
    let child = fail_pipeline_attempt(&runtime, &child_task, None, dead_pid());
    let parent_task = create_backlog_task(&runtime, &repo, "parent");
    couple_parent_to_child(&runtime, &parent_task, &child, dead_pid());

    let trigger = trigger();
    let host = Host::new(&runtime, &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    assert_eq!(page.candidates.len(), 1);
    let member = page.candidates[0].clone();
    assert_eq!(member.task_ids.len(), 2);

    runtime
        .apply_task_automation_update(
            &parent_task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Backlog),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("drop parent from the blocked cohort");

    assert_retire(
        host.admission(&member).unwrap(),
        "incident_membership_or_recovery_changed",
    );
}

#[test]
fn stale_recovery_cannot_authorize_and_material_changed_still_fires() {
    let (_root, runtime, repo) = test_runtime();
    let task = create_backlog_task(&runtime, &repo, "settled");
    let run_id = fail_pipeline_attempt(&runtime, &task, None, dead_pid());

    let trigger = trigger();
    let host = Host::new(&runtime, &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    let member = page.candidates[0].clone();
    assert!(matches!(
        host.admission(&member).unwrap(),
        MemberAdmission::Admit
    ));

    let mut stale_fingerprint = member.clone();
    stale_fingerprint.fingerprint = "not-the-incident-key".into();
    assert_retire(
        host.admission(&stale_fingerprint).unwrap(),
        "material_changed",
    );

    let retry = runtime
        .stores()
        .jobs()
        .insert_job_run(PIPELINE_JOB, 2, Utc::now(), None, Some(run_id))
        .unwrap();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&retry.run_id, Utc::now(), std::process::id())
        .unwrap();

    match host.admission(&member).unwrap() {
        MemberAdmission::Admit => panic!("stale recovery evidence authorized the candidate"),
        MemberAdmission::Retire(_) | MemberAdmission::Withhold(_) => {}
    }
}

#[test]
fn multi_candidate_admission_reuses_inventory_after_freshness_check() {
    let (_root, runtime, repo) = test_runtime();
    for hint in ["a", "b", "c"] {
        let task = create_backlog_task(&runtime, &repo, hint);
        fail_pipeline_attempt(&runtime, &task, None, dead_pid());
    }

    let trigger = trigger();
    let host = Host::new(&runtime, &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    assert_eq!(page.candidates.len(), 3);
    let after_observe = host.incident_work_stats();
    assert_eq!(
        after_observe.inventory_builds, 1,
        "observe hydrates the blocked cohort once"
    );
    assert_eq!(after_observe.inventory_reuses, 0);
    let observe_observes = after_observe.observes;
    let observe_task_reads = after_observe.task_reads;
    let observe_run_reads = after_observe.run_reads;
    let observe_probes = after_observe.liveness_probes;

    for candidate in &page.candidates {
        assert!(matches!(
            host.admission(candidate).unwrap(),
            MemberAdmission::Admit
        ));
    }

    let stats = host.incident_work_stats();
    // Reuse lifetime: one Host / evaluate. Observe may populate the session.
    // Admission reuses that snapshot only after re-listing blocked-task
    // identities and re-probing related-run owner/state, retry children,
    // child-dispatch tokens, and liveness. A changed token rebuilds.
    // Fingerprint observe of each candidate's task_ids stays a fresh read
    // (not counted in session.observes). This is not "exactly one inventory
    // per evaluation": a freshness miss rebuilds, and a later evaluation
    // uses a new Host.
    assert_eq!(
        stats.inventory_builds, 1,
        "unchanged world must not rebuild per candidate"
    );
    assert!(
        stats.inventory_reuses >= page.candidates.len(),
        "each admission should reuse after a freshness check, reuses={}",
        stats.inventory_reuses
    );
    assert_eq!(
        stats.observes, observe_observes,
        "inventory diagnose must not rerun per admission"
    );

    let naive_builds = 1 + page.candidates.len();
    let naive_inventory_observes = observe_observes * naive_builds;
    assert!(
        stats.inventory_builds < naive_builds,
        "previous path rebuilt inventory on every admission"
    );
    assert!(
        stats.observes < naive_inventory_observes,
        "previous path re-walked lineage for every blocked task on every admission"
    );
    assert!(
        stats.task_reads < observe_task_reads * naive_builds,
        "freshness listing is cheaper than hydrating every blocked task again"
    );
    assert!(
        stats.run_reads < observe_run_reads * naive_builds
            || stats.liveness_probes < observe_probes * naive_builds,
        "freshness re-reads known runs and re-probes liveness without repeating the full lineage walk"
    );
}

#[test]
fn live_owner_keeps_incomplete_cohort_from_certifying_coverage() {
    let (_root, runtime, repo) = test_runtime();
    let child_task = create_backlog_task(&runtime, &repo, "child");
    let child = fail_pipeline_attempt(&runtime, &child_task, None, dead_pid());
    let parent_task = create_backlog_task(&runtime, &repo, "live-parent");
    couple_parent_to_child(&runtime, &parent_task, &child, std::process::id());

    let trigger = trigger();
    let host = Host::new(&runtime, &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    assert!(
        page.candidates.is_empty(),
        "an unsettled member must not certify a partial cohort"
    );
    assert!(
        page.withheld
            .values()
            .any(|reason| reason == "incident_recovery_pending"),
        "incomplete recovery stays withheld: {:?}",
        page.withheld
    );
}
