//! Duplicate pilot preparation is a successful skip, state routines wait for
//! active holds to end, and the source pins their attempts take are released
//! once nothing can need them, by the one Orbit root and workspace that owns
//! them. Fixtures run in isolated children with real stores and a real
//! repository.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use orbit_automation::delivery::digest;
use orbit_core::application::automation::{
    consumer_key, evaluate_routine, pin_attempt_source, release_unreferenced_attempt_pins,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, Task, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{JobRunStoreBackend, KeyedJobRunAdmission};
use orbit_tools::ToolContext;
use orbit_types::workflow::automation::AutomationState;
use orbit_types::workflow::automation::members::{MemberAttempt, StateTriggerKind};
use orbit_types::workflow::{JobRunState, PipelineState, RoutineDefinition};
use serde_json::{Value, json};
use tempfile::TempDir;

mod admission;
mod ci_sweep_races;
mod creation;
mod crew_selection;
mod native_os;
mod pilot_output;
mod races;
mod settlement;
mod source_moves;

struct Workspace {
    root: TempDir,
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
        let runtime = runtime_at(&global, &repo.join(".orbit"));
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            root,
            runtime,
            repo,
            jobs,
        }
    }

    /// Another Orbit root over this checkout with the same machine identity,
    /// as a copied or moved root would have.
    fn other_root(&self, name: &str) -> OrbitRuntime {
        let global = self.root.path().join(name).join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        runtime_at(&global, &self.repo.join(".orbit"))
    }

    /// A second workspace of this Orbit root in a linked worktree, sharing
    /// this repository's Git common directory.
    fn linked_workspace(&self) -> OrbitRuntime {
        let checkout = self.root.path().join("linked");
        self.git(&[
            "worktree",
            "add",
            "-b",
            "linked",
            checkout.to_str().unwrap(),
        ]);
        std::fs::create_dir_all(checkout.join(".orbit")).unwrap();
        std::fs::copy(
            self.repo.join(".orbit/config.toml"),
            checkout.join(".orbit/config.toml"),
        )
        .unwrap();
        runtime_at(&self.runtime.global_root(), &checkout.join(".orbit"))
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

    /// Track `path` at `contents` in a new commit on `main`.
    fn commit_file(&self, path: &str, contents: &str, message: &str) {
        let full = self.repo.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, contents).unwrap();
        self.git(&["add", "--", path]);
        self.git(&["commit", "-m", message]);
    }

    /// Every attempt and batch pin: owned, legacy and delivery.
    fn pins(&self) -> Vec<String> {
        self.git(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/orbit/pins/",
            "refs/orbit/automation/",
        ])
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn object(&self, refname: &str) -> String {
        self.git(&["rev-parse", refname]).trim().to_string()
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
        self.admitted_batch(&[task], max_attempts)
    }

    /// [`Self::admitted`] for one attempt over several pending members.
    fn admitted_batch(&self, tasks: &[&Task], max_attempts: u32) -> MemberAttempt {
        let store = self.runtime.automation_store().unwrap();
        let state = self.routine_state();
        let members = tasks
            .iter()
            .map(|task| state.members.as_ref().unwrap().pending[&task.id].clone())
            .collect::<Vec<_>>();
        let keys = tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        let id = digest(format!("attempt:{}", keys.join(",")).as_bytes());
        let now = Utc::now();
        let mut attempt = MemberAttempt {
            consumer: state.consumer.clone(),
            kind: StateTriggerKind::PreparationEligible,
            action_key: format!("automation:{id}:1"),
            id,
            member: members[0].clone(),
            members,
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

        let KeyedJobRunAdmission::Admitted(run) = self
            .jobs
            .insert_automation_job_run(
                "task_pilot_pipeline",
                json!({"state_automation": attempt}),
                &attempt.action_key,
            )
            .unwrap()
        else {
            panic!("a fresh fixture attempt must admit its own run");
        };
        attempt.action_id = Some(run.run_id);
        let mut acknowledged = claimed.clone();
        acknowledged.generation += 1;
        acknowledged.members.as_mut().unwrap().active = Some(attempt.clone());
        assert!(
            store
                .automation_commit(&claimed, &acknowledged, None)
                .unwrap()
        );
        pin_attempt_source(&self.runtime, &attempt).unwrap();
        attempt
    }

    /// Settle `attempt` with every member applied, as a later pass does.
    fn settle(&self, attempt: &MemberAttempt, at: chrono::DateTime<Utc>) {
        self.applied(attempt);
        evaluate_routine(&self.runtime, &pilot_routine(), false, at).unwrap();
        let run_id = attempt.action_id.as_deref().unwrap();
        self.jobs
            .mark_job_run_running(run_id, Utc::now(), std::process::id())
            .unwrap();
        self.jobs
            .finalize_job_run(run_id, JobRunState::Success, Utc::now(), None)
            .unwrap();
    }

    /// Record the deterministic apply step's evidence for every member.
    fn applied(&self, attempt: &MemberAttempt) {
        self.applied_with_fingerprint(attempt, None);
    }

    fn applied_with_fingerprint(
        &self,
        attempt: &MemberAttempt,
        resulting_fingerprint: Option<&str>,
    ) {
        let run_id = attempt.action_id.clone().unwrap();
        let evidence = attempt
            .members()
            .iter()
            .map(|member| {
                json!({
                    "action_id": "", "attempt_id": attempt.id, "member_key": member.key,
                    "input_fingerprint": member.fingerprint,
                    "resulting_fingerprint": resulting_fingerprint.unwrap_or(&member.fingerprint),
                    "ready": true, "result": {"task_id": member.key},
                })
            })
            .collect::<Vec<_>>();
        self.record_steps(&run_id, &[("apply", &json!({"member_evidence": evidence}))]);
    }

    /// The global index the engine checkpoints step `id` of the installed
    /// pilot job at: its position among the job's top-level steps.
    fn pilot_step_index(&self, id: &str) -> u32 {
        let path = self
            .runtime
            .global_root()
            .join("resources/jobs/task_pilot_pipeline.yaml");
        let job: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let position = job["spec"]["steps"]
            .as_sequence()
            .unwrap()
            .iter()
            .position(|step| step["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("the installed pilot job has no step `{id}`"));
        u32::try_from(position).unwrap()
    }

    /// Record each output as a successful checkpoint of `run_id`, the way
    /// the engine does: at the step's index and under its id.
    fn record_steps(&self, run_id: &str, steps: &[(&str, &Value)]) {
        let mut state = self
            .runtime
            .read_run_state(run_id)
            .unwrap()
            .unwrap_or_else(|| {
                PipelineState::new(run_id.into(), "task_pilot_pipeline".into(), json!({}))
            });
        for (id, output) in steps {
            let index = self.pilot_step_index(id);
            state.record_step(index, JobRunState::Success, Some((*output).clone()), None);
            state.record_pipeline_output(id, (*output).clone());
        }
        self.runtime.write_run_state(run_id, &state).unwrap();
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

fn runtime_at(global: &Path, orbit_dir: &Path) -> OrbitRuntime {
    OrbitRuntime::from_roots(global, orbit_dir)
        .unwrap()
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
        .with_automation_machine_identity(Some("fixture-machine".into()))
}

/// The owner record ref of the namespace `pin` lives in.
fn owner_record(pin: &str) -> String {
    let owner = pin
        .strip_prefix("refs/orbit/pins/v1/")
        .and_then(|rest| rest.split_once('/'))
        .unwrap()
        .0;
    format!("refs/orbit/pin-owners/v1/{owner}")
}

/// The namespace `pin` lives in, with its trailing slash.
fn namespace(pin: &str) -> &str {
    &pin[..=pin.rfind('/').unwrap()]
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

/// Freshness that makes repository instructions the only material field.
fn instructions_routine() -> RoutineDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 1, "name": "fixture-pilot", "enabled": true,
        "target": "job:task_pilot_pipeline",
        "trigger": {"state": {
            "kind": "preparation_eligible", "owner_machine": "fixture-machine", "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
            "retries": 1, "deadline_minutes": 90,
            "freshness": {"material_fields": ["instructions"]},
        }},
    }))
    .unwrap()
}

fn pending_fingerprint(
    workspace: &Workspace,
    routine: &RoutineDefinition,
    task_id: &str,
    at: chrono::DateTime<Utc>,
) -> String {
    // Dry-run still observes the current fingerprint. A due member must not
    // have to admit `task_pilot_pipeline` for this check.
    let evaluated = evaluate_routine(&workspace.runtime, routine, true, at).unwrap();
    evaluated
        .state
        .expect("evaluation records member state")
        .members
        .expect("a state routine has members")
        .pending
        .get(task_id)
        .unwrap_or_else(|| panic!("{task_id} should stay pending"))
        .fingerprint
        .clone()
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
    // Pinning again is idempotent and names the same owned ref.
    let pin = pin_attempt_source(&workspace.runtime, &attempt).unwrap();
    assert!(pin.starts_with("refs/orbit/pins/v1/"), "{pin}");
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

/// Record a `material_v1` assessment of `task` at `attempt`'s source: no task
/// dependencies, the resolved crew assignment, and an empty instruction list.
/// That list matches a pinned tree with no `AGENTS.md` or `CLAUDE.md`. A tree
/// that has them no longer matches, so the assessment is piloted again.
fn applied_legacy(workspace: &Workspace, task: &Task, attempt: &MemberAttempt) -> String {
    let assignment = workspace
        .runtime
        .lookup_crew_for_task(None, task.crew.as_deref())
        .unwrap();
    let legacy_dependencies = json!([{
        "effective_assignment": {
            "crew": assignment.name,
            "model": assignment.assignment.model,
            "provider": assignment.assignment.provider,
        }
    }]);
    let legacy_fingerprint = orbit_automation::members::preparation::legacy_fingerprint(
        task,
        &attempt.member.source.commit,
        &legacy_dependencies,
        "[]",
        &Default::default(),
    )
    .unwrap();
    workspace.applied_with_fingerprint(attempt, Some(&legacy_fingerprint));
    legacy_fingerprint
}

/// A pre-upgrade accepted assessment still needs its pinned source to be
/// compared under material_v1; current material_v2 assessments release at
/// settlement as covered above.
#[test]
fn routine_attempt_pin_stays_for_a_legacy_assessment() {
    if !super::dispatch_admission::isolated(
        "task_pilot::routine_attempt_pin_stays_for_a_legacy_assessment",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("legacy assessment");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 1);
    let pin = pin_attempt_source(&workspace.runtime, &attempt).unwrap();
    let legacy_fingerprint = applied_legacy(&workspace, &task, &attempt);

    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let members = workspace.routine_state().members.unwrap();
    assert_eq!(
        members.assessed[&task.id].resulting_fingerprint,
        legacy_fingerprint
    );
    assert_eq!(members.assessed[&task.id].receipt_id, attempt.id);
    assert!(members.active.is_none());
    assert_eq!(
        workspace.pins(),
        [pin],
        "the legacy assessment still needs its pinned revision"
    );
    let run_id = attempt.action_id.as_deref().unwrap();
    workspace
        .jobs
        .mark_job_run_running(run_id, Utc::now(), std::process::id())
        .unwrap();
    workspace
        .jobs
        .finalize_job_run(run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();

    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert_eq!(cleanup.retained_assessed, 1);
    assert!(cleanup.released.is_empty());
}

/// An attempt an earlier client admitted pinned only the legacy shared ref.
/// The current client still reads it by its exact name to carry the
/// assessment forward, and never deletes it: no owner is recorded for it.
#[test]
fn legacy_shared_pins_are_read_as_a_fallback_and_never_released() {
    if !super::dispatch_admission::isolated(
        "task_pilot::legacy_shared_pins_are_read_as_a_fallback_and_never_released",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("pinned by an earlier client");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 1);
    let owned = pin_attempt_source(&workspace.runtime, &attempt).unwrap();
    workspace.git(&["update-ref", "-d", &owned]);
    let legacy = format!("refs/orbit/automation/{}", attempt.id);
    workspace.pin(&legacy);

    applied_legacy(&workspace, &task, &attempt);
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    assert_eq!(
        workspace.pins(),
        std::slice::from_ref(&legacy),
        "settlement kept it"
    );

    // The next pass recomputes the legacy digest at the legacy pin, so the
    // unchanged task keeps its assessment instead of joining a re-pilot.
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(5),
    )
    .unwrap();
    let members = workspace.routine_state().members.unwrap();
    assert_eq!(members.assessed[&task.id].receipt_id, attempt.id);
    assert!(
        !members.pending.contains_key(&task.id),
        "the legacy fallback carried the assessment forward"
    );

    let run_id = attempt.action_id.as_deref().unwrap();
    workspace
        .jobs
        .mark_job_run_running(run_id, Utc::now(), std::process::id())
        .unwrap();
    workspace
        .jobs
        .finalize_job_run(run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert_eq!(cleanup.retained_legacy, 1);
    assert!(cleanup.released.is_empty());
    assert_eq!(workspace.pins(), [legacy]);
}

/// Tracked root and nested `AGENTS.md` and `CLAUDE.md` bytes are part of the
/// preparation fingerprint when `instructions` is material. A near-name, an
/// ordinary file, and an untracked instruction file are not.
#[test]
fn instruction_edits_change_the_preparation_fingerprint() {
    if !super::dispatch_admission::isolated(
        "task_pilot::instruction_edits_change_the_preparation_fingerprint",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("instruction material");
    let routine = instructions_routine();
    let now = Utc::now();
    let empty = pending_fingerprint(&workspace, &routine, &task.id, now);

    workspace.commit_file(
        " instructions/AGENTS.md",
        "spaced directory rules\n",
        "spaced directory instructions",
    );
    workspace.commit_file("AGENTS.md", "root rules\n", "root instructions");
    workspace.commit_file("a/AGENTS.md", "nested agents\n", "nested agents");
    workspace.commit_file("a/b/CLAUDE.md", "nested claude\n", "nested claude");
    workspace.commit_file("docs/guide.md", "ordinary\n", "ordinary doc");
    workspace.commit_file("docs/AGENTS.md.bak", "not an instruction\n", "near name");
    let listed = pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(1));
    assert_ne!(
        listed, empty,
        "tracked instruction files, including a leading-space path, enter the snapshot"
    );

    workspace.commit_file("AGENTS.md", "root rules revised\n", "edit root");
    let root_edited =
        pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(2));
    assert_ne!(
        root_edited, listed,
        "a root AGENTS.md edit changes the fingerprint"
    );

    workspace.commit_file(
        "a/AGENTS.md",
        "nested agents revised\n",
        "edit nested agents",
    );
    let nested_agents =
        pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(3));
    assert_ne!(
        nested_agents, root_edited,
        "a nested AGENTS.md edit changes the fingerprint"
    );

    workspace.commit_file(
        "a/b/CLAUDE.md",
        "nested claude revised\n",
        "edit nested claude",
    );
    let nested_claude =
        pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(4));
    assert_ne!(
        nested_claude, nested_agents,
        "a nested CLAUDE.md edit changes the fingerprint"
    );

    workspace.commit_file(
        "AGENTS.md",
        "    root rules revised\n",
        "indent root instructions",
    );
    let indented_root =
        pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(5));
    assert_ne!(
        indented_root, nested_claude,
        "leading instruction whitespace is part of the pinned content"
    );

    workspace.commit_file("docs/guide.md", "ordinary revised\n", "edit ordinary");
    workspace.commit_file(
        "docs/AGENTS.md.bak",
        "still not an instruction\n",
        "edit near name",
    );
    let unchanged = pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(6));
    assert_eq!(
        unchanged, indented_root,
        "only AGENTS.md and CLAUDE.md basenames are instruction material"
    );

    let untracked = workspace.repo.join("scratch/CLAUDE.md");
    std::fs::create_dir_all(untracked.parent().unwrap()).unwrap();
    std::fs::write(&untracked, "untracked rules\n").unwrap();
    let still = pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(7));
    assert_eq!(
        still, unchanged,
        "an untracked instruction file is not in the pinned snapshot"
    );
}

/// A tree whose full path listing is larger than the 1 MiB cap on one
/// command's output still yields its instruction files: the listing is
/// filtered as it is read, not buffered as evidence.
#[test]
fn instruction_snapshot_survives_a_tree_listing_over_the_source_output_cap() {
    if !super::dispatch_admission::isolated(
        "task_pilot::instruction_snapshot_survives_a_tree_listing_over_the_source_output_cap",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("large tree instruction material");
    let routine = instructions_routine();
    let now = Utc::now();
    let empty = pending_fingerprint(&workspace, &routine, &task.id, now);

    let bulk = workspace.repo.join("bulk").join("d".repeat(100));
    std::fs::create_dir_all(&bulk).unwrap();
    for index in 0..10_000 {
        std::fs::write(bulk.join(format!("file_{index:05}.txt")), "x\n").unwrap();
    }
    workspace.commit_file("AGENTS.md", "root rules\n", "root instructions");
    workspace.commit_file("bulk/nested/CLAUDE.md", "nested claude\n", "nested claude");
    workspace.git(&["add", "-A"]);
    workspace.git(&["commit", "-m", "bulk files"]);
    let listing = workspace
        .git(&["ls-tree", "-r", "-z", "--name-only", "--full-tree", "HEAD"])
        .len();
    assert!(
        listing > 1_048_576,
        "the fixture must list more than the source output cap, got {listing} bytes"
    );

    let listed = pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(1));
    assert_ne!(
        listed, empty,
        "instruction files of an oversized tree enter the snapshot"
    );

    workspace.commit_file(
        "bulk/nested/CLAUDE.md",
        "nested claude revised\n",
        "edit nested claude",
    );
    let edited = pending_fingerprint(&workspace, &routine, &task.id, now + Duration::minutes(2));
    assert_ne!(
        edited, listed,
        "an instruction edit deep in an oversized tree changes the fingerprint"
    );
}

/// A `material_v1` assessment stored against an empty instruction snapshot
/// does not carry forward once the pinned revision's tracked instruction
/// files are part of that hash. The member is piloted again once.
#[test]
fn legacy_assessment_repilots_when_pinned_instructions_were_omitted() {
    if !super::dispatch_admission::isolated(
        "task_pilot::legacy_assessment_repilots_when_pinned_instructions_were_omitted",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    workspace.commit_file("AGENTS.md", "root rules\n", "root instructions");
    workspace.commit_file("a/AGENTS.md", "nested agents\n", "nested agents");
    workspace.commit_file("a/b/CLAUDE.md", "nested claude\n", "nested claude");
    let task = workspace.task("legacy empty snapshot");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 1);
    let stale = applied_legacy(&workspace, &task, &attempt);

    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let members = workspace.routine_state().members.unwrap();
    assert_eq!(
        members.assessed[&task.id].resulting_fingerprint, stale,
        "settlement records the empty-snapshot assessment"
    );
    assert!(
        workspace.pins().is_empty(),
        "that hash does not match the pinned instructions, so the pin is not kept"
    );

    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(5),
    )
    .unwrap();
    let members = workspace.routine_state().members.unwrap();
    assert!(
        members.pending.contains_key(&task.id),
        "the member is piloted again once"
    );
    assert_ne!(
        members.pending[&task.id].fingerprint, stale,
        "the new fingerprint is not the empty-snapshot legacy hash"
    );
}

/// `orbit doctor --fix-automation-pins` reclaims leaked owned pins past the
/// size one namespace listing could read, and keeps every pin a consumer or
/// live run names, every legacy and delivery batch pin, and every ref it
/// cannot prove is an attempt pin. It refuses while a sweep holds the lock.
#[test]
fn fix_automation_pins_releases_only_owned_attempt_pins_nothing_names() {
    if !super::dispatch_admission::isolated(
        "task_pilot::fix_automation_pins_releases_only_owned_attempt_pins_nothing_names",
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
    workspace.settle(&settled, now + Duration::minutes(3));
    let assessed_pin = pin_attempt_source(&workspace.runtime, &settled).unwrap();
    let owned = namespace(&assessed_pin).to_string();

    let active = workspace.task("active");
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let in_flight = workspace.admitted(&active, 1);
    let active_pin = pin_attempt_source(&workspace.runtime, &in_flight).unwrap();

    let live = MemberAttempt {
        id: digest(b"live run attempt"),
        ..in_flight.clone()
    };
    workspace
        .jobs
        .insert_job_run(
            "task_pilot_pipeline",
            1,
            Utc::now(),
            Some(json!({"state_automation": {"id": live.id}})),
            None,
        )
        .unwrap();
    let live_pin = pin_attempt_source(&workspace.runtime, &live).unwrap();

    let batch_pin = format!("refs/orbit/automation/{}/batch/from", digest(b"consumer"));
    workspace.pin(&batch_pin);
    let legacy_pin = format!("refs/orbit/automation/{}", digest(b"legacy attempt"));
    workspace.pin(&legacy_pin);
    let unrecognized = [
        "refs/orbit/automation/abc-not-an-attempt".to_string(),
        format!("{owned}abc-not-an-attempt"),
    ];
    for refname in &unrecognized {
        workspace.pin(refname);
    }

    // More leaked owned pins than one 1 MiB listing of the namespace holds.
    let head = workspace.git(&["rev-parse", "HEAD"]);
    let leaked = (0..9_000)
        .map(|index| format!("{owned}{}", digest(format!("leaked {index}").as_bytes())))
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
    assert_eq!(before.len(), leaked.len() + 7);

    let lock =
        orbit_store::try_acquire_routine_sweep_lock(&workspace.runtime.global_root().join("state"))
            .unwrap()
            .unwrap();
    assert!(release_unreferenced_attempt_pins(&workspace.runtime).is_err());
    assert_eq!(workspace.pins(), before, "a refused repair deletes nothing");
    drop(lock);

    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert_eq!(cleanup.namespace, owned);
    assert_eq!(cleanup.refused, None);
    let mut released = cleanup.released.clone();
    released.sort();
    let mut expected = leaked.clone();
    expected.sort();
    assert_eq!(released, expected);
    assert_eq!(
        (
            cleanup.retained_active,
            cleanup.retained_assessed,
            cleanup.retained_live_run,
            cleanup.retained_legacy,
            cleanup.foreign_owners,
        ),
        (1, 1, 1, 1, 0)
    );
    let mut reported = cleanup.unrecognized.clone();
    reported.sort();
    assert_eq!(reported, unrecognized);
    assert!(cleanup.kept.is_empty());
    let mut remaining = vec![assessed_pin, active_pin, live_pin, batch_pin, legacy_pin];
    remaining.extend(unrecognized);
    remaining.sort();
    assert_eq!(workspace.pins(), remaining);

    let again = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert!(again.released.is_empty(), "the repair is idempotent");
}

/// Orbit roots and workspaces sharing one Git common directory pin the same
/// attempt — identical machine, consumer and attempt ids — in disjoint
/// namespaces. Each releases and reclaims only its own; a canonical alias of
/// a root is that root, and a second root (as a copy or move would be) never
/// adopts the first one's pins.
#[test]
fn owned_pins_are_disjoint_across_roots_and_workspaces_sharing_git() {
    if !super::dispatch_admission::isolated(
        "task_pilot::owned_pins_are_disjoint_across_roots_and_workspaces_sharing_git",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("shared");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 1);
    let pin = pin_attempt_source(&workspace.runtime, &attempt).unwrap();

    #[cfg(unix)]
    {
        let alias = workspace.root.path().join("alias");
        std::os::unix::fs::symlink(workspace.root.path().join("home"), &alias).unwrap();
        let aliased = runtime_at(&alias.join(".orbit"), &workspace.repo.join(".orbit"));
        assert_eq!(pin_attempt_source(&aliased, &attempt).unwrap(), pin);
    }

    let other_root = workspace.other_root("other-home");
    let linked = workspace.linked_workspace();
    assert_ne!(
        linked.workspace_id().unwrap(),
        workspace.runtime.workspace_id().unwrap()
    );
    let other_pin = pin_attempt_source(&other_root, &attempt).unwrap();
    let linked_pin = pin_attempt_source(&linked, &attempt).unwrap();
    let distinct = [&pin, &other_pin, &linked_pin]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(distinct.len(), 3, "{distinct:?}");
    for refname in [&pin, &other_pin, &linked_pin] {
        assert_eq!(workspace.object(refname), attempt.member.source.commit);
    }

    workspace.settle(&attempt, now + Duration::minutes(3));
    let mut expected = vec![other_pin.clone(), linked_pin.clone()];
    expected.sort();
    assert_eq!(
        workspace.pins(),
        expected,
        "settlement released only its own"
    );

    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert!(cleanup.released.is_empty());
    assert_eq!(cleanup.foreign_owners, 2);
    assert_eq!(workspace.pins(), expected, "foreign pins are never touched");

    // Nothing in the linked workspace's or the other root's stores names the
    // attempt, so each owner reclaims its own pin and only that one.
    let cleanup = release_unreferenced_attempt_pins(&linked).unwrap();
    assert_eq!(cleanup.released, [linked_pin]);
    assert_eq!(workspace.pins(), std::slice::from_ref(&other_pin));
    let cleanup = release_unreferenced_attempt_pins(&other_root).unwrap();
    assert_eq!(cleanup.released, [other_pin]);
    assert!(workspace.pins().is_empty());
}

/// A namespace whose owner record is missing or names another owner is not
/// provably this owner's: admission refuses to pin into it, and neither
/// settlement nor the repair deletes anything in it. A pin that names a
/// different commit is never rebound or released by its attempt.
#[test]
fn unproven_ownership_and_moved_pins_retain_every_pin() {
    if !super::dispatch_admission::isolated(
        "task_pilot::unproven_ownership_and_moved_pins_retain_every_pin",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("conflicting owner");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 1);
    let pin = pin_attempt_source(&workspace.runtime, &attempt).unwrap();
    let record = owner_record(&pin);
    let bound = workspace.object(&record);

    let foreign = git_in(
        &workspace.repo,
        &["hash-object", "-w", "--stdin"],
        Some(
            r#"{"schema":1,"root":"/elsewhere","workspace":"other","machine":"fixture-machine","git_common_dir":"/elsewhere/.git"}"#,
        ),
    );
    workspace.git(&["update-ref", &record, foreign.trim()]);
    let other = MemberAttempt {
        id: digest(b"admitted under a conflicting owner"),
        ..attempt.clone()
    };
    let refused = pin_attempt_source(&workspace.runtime, &other).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("automation_pin_owner_unproven"),
        "{refused}"
    );

    workspace.settle(&attempt, now + Duration::minutes(3));
    assert_eq!(
        workspace.pins(),
        std::slice::from_ref(&pin),
        "settlement kept it"
    );
    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert!(cleanup.refused.is_some());
    assert_eq!(cleanup.retained_unproven, 1);
    assert!(cleanup.released.is_empty());

    workspace.git(&["update-ref", "-d", &record]);
    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert!(cleanup.refused.is_some(), "a missing record proves nothing");
    assert_eq!(workspace.pins(), std::slice::from_ref(&pin));

    // Restored, the record proves the namespace again; the settled
    // attempt's assessment still names the pin.
    workspace.git(&["update-ref", &record, &bound]);
    let cleanup = release_unreferenced_attempt_pins(&workspace.runtime).unwrap();
    assert_eq!(cleanup.refused, None);
    assert_eq!(cleanup.retained_assessed, 1);
    assert_eq!(workspace.pins(), std::slice::from_ref(&pin));

    // A pin repointed after admission is neither rebound nor released.
    let moved_task = workspace.task("moved pin");
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let moved = workspace.admitted(&moved_task, 1);
    let moved_pin = pin_attempt_source(&workspace.runtime, &moved).unwrap();
    std::fs::write(workspace.repo.join("README.md"), "moved\n").unwrap();
    workspace.git(&["commit", "-am", "move"]);
    let elsewhere = workspace.object("HEAD");
    workspace.git(&["update-ref", &moved_pin, &elsewhere]);
    assert!(pin_attempt_source(&workspace.runtime, &moved).is_err());
    workspace.settle(&moved, now + Duration::minutes(5));
    let mut expected = vec![pin, moved_pin.clone()];
    expected.sort();
    assert_eq!(workspace.pins(), expected);
    assert_eq!(workspace.object(&moved_pin), elsewhere);
}
