//! Host observe/admission fixtures for incident inventory reuse [ORB-11633].

use super::super::{
    consumer_key,
    members::{Host, claim, head_invocations, reset_head_invocations},
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
        ChildDispatch, ChildDispatchPhase, JobRunState, JobRunTriggerKind, JobTargetType,
        PipelineState,
        automation::{
            AutomationState, SourceRevision,
            members::{
                MemberAttempt, MemberState, PreparationEligibility, StateMember, StateTrigger,
                StateTriggerKind,
            },
        },
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
        batch_size: None,
        eligibility: PreparationEligibility::default(),
    }
}

fn preparation_trigger() -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        ..trigger()
    }
}

fn create_backlog_task(runtime: &OrbitRuntime, _repo_root: &Path, id_hint: &str) -> String {
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

fn create_proposed_task(runtime: &OrbitRuntime, _repo_root: &Path, id_hint: &str) -> String {
    create_scoped_proposed_task(runtime, id_hint, Vec::new())
}

fn create_scoped_proposed_task(
    runtime: &OrbitRuntime,
    id_hint: &str,
    context_files: Vec<String>,
) -> String {
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
            context_files,
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
    let page = Host::new(&runtime, "task-pilot", &trigger)
        .observe(None, Utc::now())
        .unwrap();

    assert_eq!(page.candidates.len(), 3);
    assert_eq!(
        ls_tree_invocations(),
        1,
        "one revision-scoped instruction listing serves the entire page"
    );
}

/// [ORB-12761] Preparation members carry stored `task.crew` so the batching
/// step can keep a dispatch bundle crew-homogeneous.
#[test]
fn preparation_observe_stamps_stored_task_crew() {
    let (_root, runtime, repo) = test_runtime();
    let opus = create_proposed_task(&runtime, &repo, "opus");
    let sol = create_proposed_task(&runtime, &repo, "sol");
    let unset = create_proposed_task(&runtime, &repo, "unset");
    runtime
        .update_task(
            &opus,
            crate::application::task::TaskUpdateParams {
                crew: Some(Some("opus".into())),
                ..Default::default()
            },
        )
        .expect("assign opus");
    runtime
        .update_task(
            &sol,
            crate::application::task::TaskUpdateParams {
                crew: Some(Some("sol".into())),
                ..Default::default()
            },
        )
        .expect("assign sol");

    let page = Host::new(&runtime, "task-pilot", &preparation_trigger())
        .observe(None, Utc::now())
        .unwrap();
    let crew_of = |id: &str| {
        page.candidates
            .iter()
            .find(|member| member.key == id)
            .map(|member| member.crew.clone())
    };
    assert_eq!(crew_of(&opus), Some(Some("opus".into())));
    assert_eq!(crew_of(&sol), Some(Some("sol".into())));
    assert_eq!(crew_of(&unset), Some(None));
}

/// [ORB-12796] An execution-failed member's `task_ids` come from incident
/// grouping, not one task's stored crew: the member carries that crew only
/// once every id in the incident agrees.
#[test]
fn execution_failed_observe_stamps_agreed_incident_crew() {
    let (_root, runtime, repo) = test_runtime();
    let child_task = create_backlog_task(&runtime, &repo, "child");
    let child = fail_pipeline_attempt(&runtime, &child_task, None, dead_pid());
    let parent_task = create_backlog_task(&runtime, &repo, "parent");
    couple_parent_to_child(&runtime, &parent_task, &child, dead_pid());

    for id in [&child_task, &parent_task] {
        runtime
            .update_task(
                id,
                crate::application::task::TaskUpdateParams {
                    crew: Some(Some("opus".into())),
                    ..Default::default()
                },
            )
            .expect("assign opus");
    }

    let trigger = trigger();
    let host = Host::new(&runtime, "task-pilot", &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    assert_eq!(page.candidates.len(), 1);
    let member = &page.candidates[0];
    assert_eq!(member.task_ids.len(), 2);
    assert_eq!(member.crew.as_deref(), Some("opus"));
}

/// [ORB-12796] Two tasks in one incident cohort with different stored crews
/// must not masquerade as an unset (`None`) crew: the batching filter that
/// keeps a dispatch bundle crew-homogeneous trusts `StateMember.crew`, so a
/// disagreeing incident is withheld instead of silently admitted.
#[test]
fn execution_failed_incident_with_mixed_crew_is_withheld() {
    let (_root, runtime, repo) = test_runtime();
    let child_task = create_backlog_task(&runtime, &repo, "child");
    let child = fail_pipeline_attempt(&runtime, &child_task, None, dead_pid());
    let parent_task = create_backlog_task(&runtime, &repo, "parent");
    couple_parent_to_child(&runtime, &parent_task, &child, dead_pid());

    runtime
        .update_task(
            &child_task,
            crate::application::task::TaskUpdateParams {
                crew: Some(Some("opus".into())),
                ..Default::default()
            },
        )
        .expect("assign opus");
    runtime
        .update_task(
            &parent_task,
            crate::application::task::TaskUpdateParams {
                crew: Some(Some("sol".into())),
                ..Default::default()
            },
        )
        .expect("assign sol");

    let trigger = trigger();
    let host = Host::new(&runtime, "task-pilot", &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    assert!(
        page.candidates.is_empty(),
        "a mixed-crew incident must not be admitted as a candidate: {:?}",
        page.candidates
    );
    assert!(
        page.withheld
            .values()
            .any(|reason| reason == "incident_mixed_crew"),
        "mixed-crew incident cohort must be withheld: {:?}",
        page.withheld
    );
}

#[test]
fn preparation_admission_resolves_branch_head_once_per_call() {
    let (_root, runtime, repo) = test_runtime();
    let id = create_proposed_task(&runtime, &repo, "once");
    let trigger = preparation_trigger();
    let host = Host::new(&runtime, "task-pilot", &trigger);
    let page = host.observe(None, Utc::now()).unwrap();
    assert_eq!(page.candidates.len(), 1);
    let mut member = page.candidates[0].clone();
    assert_eq!(member.task_ids, vec![id.clone()]);
    // Same material twice: the pre-fix loop would resolve head per id.
    member.task_ids = vec![id.clone(), id];

    reset_head_invocations();
    assert!(matches!(
        host.admission(&member).unwrap(),
        MemberAdmission::Admit
    ));
    assert_eq!(
        head_invocations(),
        1,
        "admission must resolve the branch head once per call, not per task id"
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

    let eligibility = PreparationEligibility::default();
    let before =
        preparation::fingerprint(&runtime, &task, &revision, &eligibility).expect("fingerprint");
    let instructions = preparation::instructions(&runtime, &revision).expect("instructions");
    let after = preparation::fingerprint_with_instructions(
        &runtime,
        &task,
        &revision,
        &instructions,
        &eligibility,
    )
    .expect("fingerprint with cached instructions");

    assert_eq!(before, after);
}

/// [ORB-12745] The trigger's resolved eligibility is the one predicate the
/// host observes, admits and fingerprints with: a narrowed block changes
/// which tasks are candidates versus withheld, produces a different material
/// fingerprint for the same task, and retires a member the predicate no
/// longer admits at the admission recheck.
#[test]
fn host_observes_admits_and_fingerprints_with_the_triggers_eligibility() {
    let (_root, runtime, repo) = test_runtime();
    let proposed = create_proposed_task(&runtime, &repo, "proposed");
    let backlog = create_backlog_task(&runtime, &repo, "backlog");
    let now = Utc::now();

    let default_trigger = preparation_trigger();
    let default_page = Host::new(&runtime, "task-pilot", &default_trigger)
        .observe(None, now)
        .unwrap();
    let mut default_keys: Vec<_> = default_page
        .candidates
        .iter()
        .map(|member| member.key.clone())
        .collect();
    default_keys.sort();
    let mut both = vec![proposed.clone(), backlog.clone()];
    both.sort();
    assert_eq!(default_keys, both);
    assert!(default_page.withheld.is_empty());

    let narrowed_trigger = StateTrigger {
        eligibility: PreparationEligibility {
            statuses: vec![TaskStatus::Backlog],
            require_tags: vec!["pilot".into()],
            ..Default::default()
        },
        ..preparation_trigger()
    };
    let narrowed_host = Host::new(&runtime, "task-pilot", &narrowed_trigger);
    let page = narrowed_host.observe(None, now).unwrap();
    assert!(
        page.candidates.is_empty(),
        "the backlog task lacks the required tag: {:?}",
        page.candidates
    );
    assert_eq!(
        page.withheld.get(&backlog).map(String::as_str),
        Some("task_ineligible")
    );
    assert!(
        !page.withheld.contains_key(&proposed),
        "a status outside the predicate is never listed, so it is not withheld"
    );

    runtime
        .update_task(
            &backlog,
            crate::application::task::TaskUpdateParams {
                tags: Some(vec!["pilot".into()]),
                ..Default::default()
            },
        )
        .expect("tag the backlog task");
    let page = narrowed_host.observe(None, now).unwrap();
    assert_eq!(page.candidates.len(), 1);
    let member = page.candidates[0].clone();
    assert_eq!(member.task_ids, vec![backlog.clone()]);
    assert!(matches!(
        narrowed_host.admission(&member).unwrap(),
        MemberAdmission::Admit
    ));

    // Same task, same revision, different predicate: different material.
    let default_member = Host::new(&runtime, "task-pilot", &default_trigger)
        .observe(None, now)
        .unwrap()
        .candidates
        .into_iter()
        .find(|candidate| candidate.key == backlog)
        .expect("the default predicate still lists the task");
    assert_ne!(default_member.fingerprint, member.fingerprint);

    // The predicate is re-evaluated at admission, so a task that stopped
    // satisfying it between observe and admission is retired.
    runtime
        .update_task(
            &backlog,
            crate::application::task::TaskUpdateParams {
                tags: Some(vec![]),
                ..Default::default()
            },
        )
        .expect("drop the required tag");
    assert_retire(narrowed_host.admission(&member).unwrap(), "task_ineligible");
}

#[test]
fn stale_membership_between_observe_and_admission_is_retired() {
    let (_root, runtime, repo) = test_runtime();
    let child_task = create_backlog_task(&runtime, &repo, "child");
    let child = fail_pipeline_attempt(&runtime, &child_task, None, dead_pid());
    let parent_task = create_backlog_task(&runtime, &repo, "parent");
    couple_parent_to_child(&runtime, &parent_task, &child, dead_pid());

    let trigger = trigger();
    let host = Host::new(&runtime, "task-pilot", &trigger);
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
    let host = Host::new(&runtime, "task-pilot", &trigger);
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
    let host = Host::new(&runtime, "task-pilot", &trigger);
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
    let host = Host::new(&runtime, "task-pilot", &trigger);
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

fn commit_file(repo: &Path, path: &str, contents: &str) {
    let file = repo.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, contents).unwrap();
    git(repo, &["add", path]);
    git(repo, &["commit", "-m", &format!("touch {path}")]);
}

/// How the consumer's stored attempt stands when a run rechecks its claim.
#[derive(Clone, Copy, Debug)]
enum Stored {
    Live,
    Expired,
    Exhausted,
}

/// A preparation claim frozen at the current `agent-main` head over one task
/// scoped to `context_files`, claimed and admitted as the consumer's active
/// attempt through the store's own checkpoint transitions.
fn preparation_claim(
    runtime: &OrbitRuntime,
    repo: &Path,
    context_files: &[&str],
    stored: Stored,
) -> MemberAttempt {
    let id = create_scoped_proposed_task(
        runtime,
        "prepared",
        context_files
            .iter()
            .map(|selector| selector.to_string())
            .collect(),
    );
    let source = SourceRevision {
        commit: git(repo, &["rev-parse", "HEAD"]),
        tree: git(repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let now = Utc::now();
    let (retry_after, deadline) = match stored {
        Stored::Expired => (
            now - chrono::Duration::minutes(2),
            now - chrono::Duration::minutes(1),
        ),
        Stored::Live | Stored::Exhausted => (now, now + chrono::Duration::minutes(30)),
    };
    let member = StateMember {
        key: id.clone(),
        task_ids: vec![id],
        fingerprint: "fixture-fingerprint".into(),
        source: source.clone(),
        evidence: json!({}),
        first_seen: now,
        changed_at: now,
        crew: None,
    };
    let consumer = consumer_key(runtime, "routine", "pilot").unwrap();
    let attempt = MemberAttempt {
        consumer: consumer.clone(),
        kind: StateTriggerKind::PreparationEligible,
        id: "fixture-attempt".into(),
        member: member.clone(),
        members: vec![member.clone()],
        attempt: 1,
        max_attempts: 2,
        deadline,
        retry_after,
        action_key: "fixture-key".into(),
        action_id: None,
        exhausted: false,
    };
    let state = AutomationState {
        members: Some(MemberState::default()),
        consumer,
        epoch: "fixture-epoch".into(),
        trigger: None,
        repository: "fixture".into(),
        branch: "agent-main".into(),
        generation: 0,
        baseline: source.clone(),
        observed: source.clone(),
        covered: source,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
        stall: None,
    };
    let store = runtime.automation_store().unwrap();
    assert!(store.automation_initialize(&state).unwrap());
    let mut claimed = state.clone();
    claimed.generation += 1;
    let members = claimed.members.as_mut().unwrap();
    members.pending.insert(member.key.clone(), member);
    members.active = Some(attempt);
    assert!(store.automation_commit(&state, &claimed, None).unwrap());

    let mut admitted = claimed.clone();
    admitted.generation += 1;
    let active = admitted.members.as_mut().unwrap().active.as_mut().unwrap();
    active.action_id = Some("fixture-run".into());
    active.exhausted = matches!(stored, Stored::Exhausted);
    let submitted = active.clone();
    assert!(store.automation_commit(&claimed, &admitted, None).unwrap());
    submitted
}

fn stale_reason(result: Result<Option<MemberAttempt>, orbit_common::OrbitError>) -> String {
    match result {
        Ok(_) => panic!("claim was accepted instead of refused"),
        Err(error) => error.to_string(),
    }
}

/// [ORB-12981] A drain merges into the integration branch every few
/// minutes. A merge that touches none of the prepared material must not
/// discard a pilot's finished preparation at apply.
#[test]
fn preparation_claim_survives_a_head_move_disjoint_from_its_material() {
    let (_root, runtime, repo) = test_runtime();
    commit_file(&repo, "src/prepared.rs", "fn prepared() {}");
    let submitted = preparation_claim(
        &runtime,
        &repo,
        &["file:src/prepared.rs", "dir:docs/guide"],
        Stored::Live,
    );
    commit_file(&repo, "src/unrelated.rs", "fn unrelated() {}");
    commit_file(&repo, "docs/guidebook.md", "sibling of a scoped directory");

    let prepared = json!({
        "state_automation": submitted,
        "tasks": [{"context_files_before": ["file:src/prepared.rs"]}],
    });
    let active = claim(&runtime, &prepared, &["file:src/recommended.rs".into()])
        .expect("disjoint head move keeps the claim")
        .expect("preparation claim");
    assert_eq!(active.id, submitted.id);
    assert_ne!(
        git(&repo, &["rev-parse", "agent-main"]),
        submitted.member.source.commit,
        "the fixture must actually move the head"
    );
}

/// [ORB-12981] Revalidation is not a bypass: a move that touches any
/// prepared input — a member selector, the prepared snapshot, the pilot's
/// recommendation, or repository instructions — is still stale.
#[test]
fn preparation_claim_refuses_a_head_move_touching_its_material() {
    let cases: [(&str, &[&str], &str); 5] = [
        ("src/prepared.rs", &[], "src/prepared.rs"),
        (
            "docs/guide/nested/page.md",
            &[],
            "docs/guide/nested/page.md",
        ),
        ("src/before.rs", &[], "src/before.rs"),
        (
            "src/recommended.rs",
            &["file:src/recommended.rs"],
            "src/recommended.rs",
        ),
        ("crates/nested/AGENTS.md", &[], "repository instructions"),
    ];
    for (touched, material, expected) in cases {
        let (_root, runtime, repo) = test_runtime();
        let submitted = preparation_claim(
            &runtime,
            &repo,
            &["symbol:src/prepared.rs#prepared:function", "dir:docs/guide"],
            Stored::Live,
        );
        commit_file(&repo, touched, "changed after preparation");
        let prepared = json!({
            "state_automation": submitted,
            "tasks": [{"context_files_before": ["file:src/before.rs"]}],
        });
        let material = material.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let reason = stale_reason(claim(&runtime, &prepared, &material));
        assert!(
            reason.contains("stale preparation") && reason.contains(expected),
            "touching {touched} must refuse as stale naming {expected}: {reason}"
        );
    }
}

#[test]
fn preparation_claim_refuses_a_rewritten_branch_or_unanchored_material() {
    let (_root, runtime, repo) = test_runtime();
    let submitted = preparation_claim(&runtime, &repo, &["file:sample.txt"], Stored::Live);
    git(&repo, &["commit", "--amend", "-m", "rewritten baseline"]);
    let reason = stale_reason(claim(
        &runtime,
        &json!({"state_automation": submitted}),
        &[],
    ));
    assert!(reason.contains("no longer descends"), "{reason}");

    let (_root, runtime, repo) = test_runtime();
    let submitted = preparation_claim(
        &runtime,
        &repo,
        &["module:orbit_core::automation"],
        Stored::Live,
    );
    commit_file(&repo, "src/unrelated.rs", "fn unrelated() {}");
    let reason = stale_reason(claim(
        &runtime,
        &json!({"state_automation": submitted}),
        &[],
    ));
    assert!(reason.contains("no repository path to compare"), "{reason}");
}

/// Identity, budget and deadline still govern the claim whether or not the
/// head moved: revalidating material never revives a dead attempt.
#[test]
fn expired_exhausted_or_mismatched_preparation_claims_are_still_refused() {
    let cases = [
        ("expired", Stored::Expired, false),
        ("exhausted", Stored::Exhausted, false),
        ("mismatched member", Stored::Live, true),
    ];
    for (label, stored, mismatched) in cases {
        for head_moved in [false, true] {
            let (_root, runtime, repo) = test_runtime();
            let mut submitted = preparation_claim(&runtime, &repo, &["file:sample.txt"], stored);
            if mismatched {
                submitted.member.fingerprint = "other-fingerprint".into();
                submitted.members = vec![submitted.member.clone()];
            }
            if head_moved {
                commit_file(&repo, "src/unrelated.rs", "fn unrelated() {}");
            }
            let reason = stale_reason(claim(
                &runtime,
                &json!({"state_automation": submitted}),
                &[],
            ));
            assert!(
                reason.contains("state claim stale or expired"),
                "{label} claim (head moved: {head_moved}) must be refused: {reason}"
            );
        }
    }
}

/// Clears the thread's pipeline worker override when the test ends.
struct IdleWorker;

impl IdleWorker {
    fn install() -> Self {
        crate::application::job::pipeline::worker_command_override::set(["sh", "-c", "sleep 1"]);
        Self
    }
}

impl Drop for IdleWorker {
    fn drop(&mut self) {
        crate::application::job::pipeline::worker_command_override::clear();
    }
}

/// [ORB-13016] A run the state consumer admits names its routine and consumer
/// as the trigger instead of reading as a CLI launch.
#[test]
fn admitted_state_run_records_its_routine_and_consumer_as_trigger() {
    let (_root, runtime, repo) = test_runtime();
    let jobs_dir = runtime.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("task_pilot_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_pilot_pipeline\nspec:\n  \
         state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      spec:\n        \
         type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .unwrap();
    let _worker = IdleWorker::install();
    let attempt = preparation_claim(&runtime, &repo, &["file:sample.txt"], Stored::Live);
    let trigger = preparation_trigger();

    let run_id = Host::new(&runtime, "pilot", &trigger)
        .admit(&attempt)
        .expect("admit the claimed attempt");

    let recorded = runtime
        .read_run_state(&run_id)
        .unwrap()
        .and_then(|state| state.trigger)
        .expect("trigger recorded");
    assert_eq!(recorded.kind, JobRunTriggerKind::Routine);
    assert_eq!(recorded.routine.as_deref(), Some("pilot"));
    assert_eq!(
        recorded.consumer.as_deref(),
        Some(attempt.consumer.as_str())
    );
    assert_eq!(recorded.slot, None);
}
