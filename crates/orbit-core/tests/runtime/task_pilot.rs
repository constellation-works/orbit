//! Duplicate pilot preparation is a successful skip, state routines wait for
//! active holds to end, and the source pins their attempts take are released
//! once nothing can need them. Fixtures run in isolated children with real
//! stores and a real repository.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use orbit_automation::delivery::digest;
use orbit_core::application::automation::{
    consumer_key, evaluate_routine, release_unreferenced_attempt_pins,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, Task, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_tools::ToolContext;
use orbit_types::workflow::automation::AutomationState;
use orbit_types::workflow::automation::members::{MemberAttempt, StateTriggerKind};
use orbit_types::workflow::{JobRunState, PipelineState, RoutineDefinition};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Workspace {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Workspace {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            repo.join(".orbit/config.toml"),
            "[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\nsystem_crew = \"fixture\"\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            git_in(&repo, args, None);
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Orbit Test"]);
        git(&["config", "user.email", "orbit-test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "seed"]);
        let activities = global.join("resources/activities");
        std::fs::create_dir_all(&activities).unwrap();
        for name in [
            "prepare_task_pilot",
            "task_pilot",
            "apply_task_pilot_results",
            "pipeline_success_guard",
        ] {
            let file = format!("{name}.yaml");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/activities")
                    .join(&file),
                activities.join(file),
            )
            .unwrap();
        }
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some("fixture-machine".into()));
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            runtime,
            repo,
            jobs,
        }
    }

    /// Runs an attempt carries resolve their catalog definition when read.
    fn install_pilot_job(&self) {
        let jobs = self.runtime.global_root().join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_pilot_pipeline.yaml"),
            jobs.join("task_pilot_pipeline.yaml"),
        )
        .unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.repo, args, None)
    }

    /// Every ref under the automation pin namespace.
    fn pins(&self) -> Vec<String> {
        self.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/orbit/automation/",
        ])
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn pin(&self, refname: &str) {
        let head = self.git(&["rev-parse", "HEAD"]);
        self.git(&["update-ref", refname, head.trim()]);
    }

    fn routine_state(&self) -> AutomationState {
        let consumer = consumer_key(&self.runtime, "routine", "fixture-pilot").unwrap();
        self.runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
    }

    /// Claim and acknowledge an attempt for `task`'s pending member through
    /// the store's own transitions, pinning its source the way admission
    /// does, with the run's input carrying the claim.
    fn admitted(&self, task: &Task, max_attempts: u32) -> MemberAttempt {
        let store = self.runtime.automation_store().unwrap();
        let state = self.routine_state();
        let member = state.members.as_ref().unwrap().pending[&task.id].clone();
        let id = digest(format!("attempt:{}", task.id).as_bytes());
        let now = Utc::now();
        let mut attempt = MemberAttempt {
            consumer: state.consumer.clone(),
            kind: StateTriggerKind::PreparationEligible,
            action_key: format!("automation:{id}:1"),
            id,
            member: member.clone(),
            members: vec![member],
            attempt: 1,
            max_attempts,
            deadline: now + Duration::minutes(90),
            retry_after: now,
            action_id: None,
            exhausted: false,
        };
        let mut claimed = state.clone();
        claimed.generation += 1;
        claimed.members.as_mut().unwrap().active = Some(attempt.clone());
        assert!(store.automation_commit(&state, &claimed, None).unwrap());

        let run = self
            .jobs
            .insert_automation_job_run(
                "task_pilot_pipeline",
                json!({"state_automation": attempt}),
                &attempt.action_key,
            )
            .unwrap();
        attempt.action_id = Some(run.run_id);
        let mut acknowledged = claimed.clone();
        acknowledged.generation += 1;
        acknowledged.members.as_mut().unwrap().active = Some(attempt.clone());
        assert!(
            store
                .automation_commit(&claimed, &acknowledged, None)
                .unwrap()
        );
        self.git(&[
            "update-ref",
            &format!("refs/orbit/automation/{}", attempt.id),
            &attempt.member.source.commit,
        ]);
        attempt
    }

    /// Record the deterministic apply step's evidence for every member.
    fn applied(&self, attempt: &MemberAttempt) {
        let run_id = attempt.action_id.clone().unwrap();
        let evidence = attempt
            .members()
            .iter()
            .map(|member| {
                json!({
                    "action_id": "", "attempt_id": attempt.id, "member_key": member.key,
                    "input_fingerprint": member.fingerprint,
                    "resulting_fingerprint": member.fingerprint,
                    "ready": true, "result": {"task_id": member.key},
                })
            })
            .collect::<Vec<_>>();
        let mut state = PipelineState::new(run_id.clone(), "task_pilot_pipeline".into(), json!({}));
        state.record_step(
            2,
            JobRunState::Success,
            Some(json!({"member_evidence": evidence})),
            None,
        );
        self.runtime.write_run_state(&run_id, &state).unwrap();
    }

    fn task(&self, title: &str) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Prepare {title}."),
                acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
                plan: "Inspect README.md.".into(),
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap()
    }

    fn action(&self, action: &str, input: Value) -> Value {
        self.runtime
            .run_deterministic(action, &json!({}), &input, ToolContext::default())
            .unwrap_or_else(|error| panic!("{action}: {error}"))
    }

    fn prepare(&self, task_ids: &[&str]) -> Value {
        self.action(
            "prepare_task_pilot",
            json!({"task_ids": task_ids, "workspace_path": self.repo, "base_branch": "main"}),
        )
    }

    /// A live pilot with a real successful prepare checkpoint; another
    /// invocation can observe this hold without starting a provider.
    fn hold(&self, task_ids: &[&str]) -> String {
        let prepared = self.prepare(task_ids);
        let run = self
            .jobs
            .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        state.record_step(0, JobRunState::Success, Some(prepared), None);
        self.runtime.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }
}

#[test]
fn all_held_pilot_run_succeeds_and_retains_every_holder_skip() {
    if !super::dispatch_admission::isolated(
        "task_pilot::all_held_pilot_run_succeeds_and_retains_every_holder_skip",
    ) {
        return;
    }
    let workspace = Workspace::new();
    // Explicit requests must not lose skips to automatic discovery's sample cap.
    let tasks = (0..21)
        .map(|index| workspace.task(&format!("held {index}")))
        .collect::<Vec<_>>();
    let ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let holder = workspace.hold(&ids);
    let job = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_pilot_pipeline.yaml");
    let run = workspace
        .runtime
        .run_job_v2_from_yaml(
            &job,
            json!({"task_ids": ids, "workspace_path": workspace.repo, "base_branch": "main"}),
        )
        .unwrap();
    assert!(run.success, "all-held selections are skips: {run:?}");
    assert_eq!(
        workspace.runtime.show_job_run(&run.run_id).unwrap().state,
        JobRunState::Success
    );
    let prepared = &run.pipeline["prepare"];
    assert_eq!(prepared["task_count"], 0);
    assert_eq!(prepared["partitions"], json!([]));
    assert_eq!(prepared["excluded_sample_truncated"], false);
    assert_eq!(prepared["excluded_by_reason"]["already_preparing"], 21);
    let expected = ids
        .iter()
        .map(|id| json!({"task_id": id, "reason": "already_preparing", "prepared_by_run_ids": [holder]}))
        .collect::<Vec<_>>();
    assert_eq!(prepared["excluded"], json!(expected));
    assert_eq!(run.pipeline["apply"]["status"], "succeeded");
    assert_eq!(
        run.pipeline["apply"]["discovery"]["excluded"],
        json!(expected)
    );
    for task in tasks {
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    }
}

#[test]
fn mixed_pilot_selection_applies_free_tasks_and_skips_held_tasks() {
    if !super::dispatch_admission::isolated(
        "task_pilot::mixed_pilot_selection_applies_free_tasks_and_skips_held_tasks",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let held = workspace.task("held");
    let free = workspace.task("free");
    let holder = workspace.hold(&[&held.id]);
    let prepared = workspace.prepare(&[&held.id, &free.id]);
    assert_eq!(prepared["task_ids"], json!([free.id]));
    assert_eq!(
        prepared["partitions"],
        json!([{"partition_index": 0, "task_ids": [free.id]}])
    );
    let skips = json!([{"task_id": held.id, "reason": "already_preparing", "prepared_by_run_ids": [holder]}]);
    assert_eq!(prepared["excluded"], skips);
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": 0, "task_ids": [free.id],
                "tasks": [{
                    "task_id": free.id,
                    "context_files_before": [], "context_files_after": ["file:README.md"],
                    "disposition": "selectors", "recommended_crew": "fixture",
                    "recommended_complexity": "low", "confidence": "high",
                    "assessment_rationale": "README.md contains the affected material.",
                    "validation_approach": "Inspect the persisted scope.",
                    "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                    "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                    "duplicate_of": null, "already_landed": null,
                }],
            }],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1);
    assert_eq!(applied["unresolved_count"], 0);
    assert_eq!(applied["discovery"]["excluded"], skips);
    assert_eq!(workspace.runtime.get_task(&held.id).unwrap(), held);
    assert_eq!(
        workspace.runtime.get_task(&free.id).unwrap().context_files,
        ["file:README.md"]
    );
}

fn git_in(repo: &Path, args: &[&str], input: Option<&str>) -> String {
    use std::io::Write;
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let mut child = command
        .args(args)
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        stdin.write_all(input.as_bytes()).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn pilot_routine() -> RoutineDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 1, "name": "fixture-pilot", "enabled": true,
        "target": "job:task_pilot_pipeline",
        "trigger": {"state": {
            "kind": "preparation_eligible", "owner_machine": "fixture-machine", "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
            "retries": 1, "deadline_minutes": 90,
        }},
    }))
    .unwrap()
}

#[test]
fn preparation_routine_withholds_active_pilot_tasks_until_the_hold_ends() {
    if !super::dispatch_admission::isolated(
        "task_pilot::preparation_routine_withholds_active_pilot_tasks_until_the_hold_ends",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("routine candidate");
    let routine = pilot_routine();
    let now = Utc::now();
    let pending = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    assert_eq!(pending.reason, "debouncing");
    assert!(
        pending
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
            .contains_key(&task.id)
    );
    let holder = workspace.hold(&[&task.id]);
    workspace
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task.id, "model": "codex", "plan": "Inspect the changed README.md material.",
                "comment": "An edit during a pilot must not start duplicate work.",
            }),
        )
        .unwrap();
    let suppressed = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(suppressed.reason, "work_withheld");
    assert!(suppressed.batch.is_empty());
    let members = suppressed.state.unwrap().members.unwrap();
    assert_eq!(
        members.withheld[&task.id],
        format!("already_preparing: {holder}")
    );
    assert!(!members.pending.contains_key(&task.id));
    assert!(members.active.is_none());
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let released = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    assert_eq!(released.reason, "debouncing");
    let members = released.state.unwrap().members.unwrap();
    assert!(members.withheld.is_empty());
    assert!(members.pending.contains_key(&task.id));
    let due = evaluate_routine(
        &workspace.runtime,
        &routine,
        true,
        now + Duration::minutes(7),
    )
    .unwrap();
    assert_eq!(due.reason, "would_fire");
    assert_eq!(due.batch.len(), 1);
    assert_eq!(due.batch[0].task_ids, std::slice::from_ref(&task.id));
    assert_eq!(workspace.prepare(&[&task.id])["task_ids"], json!([task.id]));
}

/// A retained pending member can be off the next page when a targeted pilot
/// acquires its hold. Admission must suppress it even without re-observation.
#[test]
fn preparation_routine_admission_withholds_a_held_member_off_the_scan_page() {
    if !super::dispatch_admission::isolated(
        "task_pilot::preparation_routine_admission_withholds_a_held_member_off_the_scan_page",
    ) {
        return;
    }
    let workspace = Workspace::new();
    for index in 0..50 {
        workspace
            .runtime
            .add_task(TaskAddParams {
                title: format!("opted out {index}"),
                tags: vec!["no-diff-expected".into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap();
    }
    let task = workspace.task("newest pending member");
    let routine = pilot_routine();
    let now = Utc::now();
    let observed = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let members = observed.state.unwrap().members.unwrap();
    assert!(members.pending.contains_key(&task.id));
    assert!(
        members.scan_after.is_some(),
        "the next observation must continue off this page"
    );
    let holder = workspace.hold(&[&task.id]);
    let suppressed = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(suppressed.reason, "work_withheld");
    assert!(suppressed.batch.is_empty());
    let members = suppressed.state.unwrap().members.unwrap();
    assert_eq!(
        members.withheld[&task.id],
        format!("already_preparing: {holder}")
    );
    assert!(
        members.pending.contains_key(&task.id),
        "admission preserves the deferred member"
    );
    assert!(members.active.is_none());
}

/// An attempt's source pin outlives neither its settlement nor its terminal
/// failure; before either it stays, so the run can still reach the source.
#[test]
fn routine_attempt_pins_are_released_once_the_attempt_settles_or_fails() {
    if !super::dispatch_admission::isolated(
        "task_pilot::routine_attempt_pins_are_released_once_the_attempt_settles_or_fails",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let settles = workspace.task("settles");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();

    let attempt = workspace.admitted(&settles, 2);
    let pin = format!("refs/orbit/automation/{}", attempt.id);
    let pending = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(pending.reason, "batch_pending");
    assert_eq!(
        workspace.pins(),
        std::slice::from_ref(&pin),
        "the running attempt keeps it"
    );

    workspace.applied(&attempt);
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let members = workspace.routine_state().members.unwrap();
    assert!(members.active.is_none());
    assert_eq!(members.assessed[&settles.id].receipt_id, attempt.id);
    assert!(workspace.pins().is_empty(), "settlement released {pin}");

    // Observed only now, so no pass admits it before the fixture does.
    let fails = workspace.task("fails");
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(5),
    )
    .unwrap();
    let attempt = workspace.admitted(&fails, 1);
    workspace
        .jobs
        .finalize_job_run(
            attempt.action_id.as_deref().unwrap(),
            JobRunState::Interrupted,
            Utc::now(),
            None,
        )
        .unwrap();
    let failed = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(6),
    )
    .unwrap();
    assert_eq!(failed.reason, "needs_attention");
    let members = workspace.routine_state().members.unwrap();
    assert!(members.active.is_none());
    assert!(members.failed[&fails.id].exhausted);
    assert!(workspace.pins().is_empty(), "terminal failure released it");
}

/// `orbit doctor --fix-automation-pins` releases leaked attempt pins past the
/// size one namespace listing could read, and keeps every pin a consumer or
/// live run names, every delivery batch pin, and every ref it cannot prove is
/// an attempt pin. It refuses while a sweep holds the lock.
#[test]
fn fix_automation_pins_releases_only_attempt_pins_nothing_names() {
    if !super::dispatch_admission::isolated(
        "task_pilot::fix_automation_pins_releases_only_attempt_pins_nothing_names",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let assessed = workspace.task("assessed");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();

    // A pin from before settlement released them, backing an assessment.
    let settled = workspace.admitted(&assessed, 1);
    workspace.applied(&settled);
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    let settled_run = settled.action_id.as_deref().unwrap();
    workspace
        .jobs
        .mark_job_run_running(settled_run, Utc::now(), std::process::id())
        .unwrap();
    workspace
        .jobs
        .finalize_job_run(settled_run, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let assessed_pin = format!("refs/orbit/automation/{}", settled.id);
    workspace.pin(&assessed_pin);

    let active = workspace.task("active");
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let in_flight = workspace.admitted(&active, 1);
    let active_pin = format!("refs/orbit/automation/{}", in_flight.id);

    let live = digest(b"live run attempt");
    workspace
        .jobs
        .insert_job_run(
            "task_pilot_pipeline",
            1,
            Utc::now(),
            Some(json!({"state_automation": {"id": live}})),
            None,
        )
        .unwrap();
    let live_pin = format!("refs/orbit/automation/{live}");
    workspace.pin(&live_pin);

    let batch_pin = format!("refs/orbit/automation/{}/batch/from", digest(b"consumer"));
    workspace.pin(&batch_pin);
    let unrecognized = "refs/orbit/automation/abc-not-an-attempt".to_string();
    workspace.pin(&unrecognized);

    // More leaked pins than one 1 MiB listing of the namespace can hold.
    let head = workspace.git(&["rev-parse", "HEAD"]);
    let leaked = (0..9_000)
        .map(|index| {
            format!(
                "refs/orbit/automation/{}",
                digest(format!("leaked {index}").as_bytes())
            )
        })
        .collect::<Vec<_>>();
    let instructions = leaked
        .iter()
        .map(|name| format!("create {name} {}\n", head.trim()))
        .collect::<String>();
    git_in(
        &workspace.repo,
        &["update-ref", "--stdin"],
        Some(&instructions),
    );
    let before = workspace.pins();
    assert_eq!(before.len(), leaked.len() + 5);

    let lock =
        orbit_store::try_acquire_routine_sweep_lock(&workspace.runtime.global_root().join("state"))
            .unwrap()
            .unwrap();
    assert!(release_unreferenced_attempt_pins(&workspace.runtime).is_err());
    assert_eq!(workspace.pins(), before, "a refused repair deletes nothing");
    drop(lock);

    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    let mut released = cleanup.released.clone();
    released.sort();
    let mut expected = leaked.clone();
    expected.sort();
    assert_eq!(released, expected);
    assert_eq!(
        (
            cleanup.retained_active,
            cleanup.retained_assessed,
            cleanup.retained_live_run
        ),
        (1, 1, 1)
    );
    assert_eq!(cleanup.unrecognized, std::slice::from_ref(&unrecognized));
    assert!(cleanup.kept.is_empty());
    let mut remaining = vec![assessed_pin, active_pin, live_pin, batch_pin, unrecognized];
    remaining.sort();
    assert_eq!(workspace.pins(), remaining);

    let again = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert!(again.released.is_empty(), "the repair is idempotent");
}
