//! [ORB-10801] `orbit run job` / `orbit job run` submission behaviour.
//!
//! A job run is submitted to a detached worker and the caller returns as soon
//! as the run is durable. These cover the four submission outcomes the CLI
//! distinguishes — accepted, queued, worker-startup failure, and a run that
//! fails asynchronously — plus the direct-path definition snapshot that makes
//! asynchronous execution safe for an unmanaged YAML file.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::application::job::JobRunListParams;
use crate::application::job::pipeline::TestScopeAvailability;
use crate::application::job::pipeline::{run_definition_snapshot_path, worker_command_override};

/// A finite worker for submission-path assertions. The startup observer reaps
/// it after the one-second bound, so focused test runs leave no fixture child.
const IDLE_WORKER: &str = "sleep 1";

fn test_runtime() -> (TempDir, OrbitRuntime) {
    let root = TempDir::new().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

/// A worker program that is not this test binary. Re-execing `current_exe`
/// would hand libtest the worker argv as test filters and recurse.
struct WorkerOverride;

impl WorkerOverride {
    fn shell(script: &str) -> Self {
        worker_command_override::set(["sh", "-c", script]);
        Self
    }

    fn missing_program() -> Self {
        worker_command_override::set(["/nonexistent/orbit-pipeline-worker"]);
        Self
    }
}

impl Drop for WorkerOverride {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}

/// How long a held worker waits for its release before exiting on its own, so
/// a fixture whose test never releases it cannot outlive the test for long.
const HELD_WORKER_SELF_RELEASE: Duration = Duration::from_secs(15);

/// A worker that stays alive until the test releases it, marking when it
/// started and — as its last act — when it exited. Dropping the fixture
/// releases the worker and waits for the startup observer to reap it, so both
/// a passing and a panicking test leave no child behind.
struct HeldWorker<'a> {
    runtime: &'a OrbitRuntime,
    job_name: &'a str,
    dir: TempDir,
    _override: WorkerOverride,
}

impl<'a> HeldWorker<'a> {
    fn install(runtime: &'a OrbitRuntime, job_name: &'a str) -> Self {
        let dir = TempDir::new().expect("held worker dir");
        let polls = HELD_WORKER_SELF_RELEASE.as_millis() / 50;
        let script = format!(
            "cd '{dir}' && : > started && i=0 && \
             while [ ! -e release ] && [ \"$i\" -lt {polls} ]; do sleep 0.05; i=$((i+1)); done; \
             : > exited",
            dir = dir.path().display(),
        );
        let _override = WorkerOverride::shell(&script);
        Self {
            runtime,
            job_name,
            dir,
            _override,
        }
    }

    fn marker(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }

    fn exited(&self) -> bool {
        self.marker("exited").exists()
    }

    fn wait_until_started(&self) -> bool {
        poll_until(Duration::from_secs(10), || self.marker("started").exists())
    }

    /// Let the worker exit, then wait until the observer has reaped it: it
    /// terminalizes the run only after collecting the child's exit status.
    fn release_and_reap(&self) -> bool {
        if std::fs::write(self.marker("release"), b"").is_err() {
            return false;
        }
        poll_until(HELD_WORKER_SELF_RELEASE + Duration::from_secs(5), || {
            self.runtime
                .list_job_runs_observed(JobRunListParams {
                    job_id: Some(self.job_name.to_string()),
                    ..Default::default()
                })
                .is_ok_and(|runs| runs.iter().all(|run| run.state.is_terminal()))
        })
    }
}

impl Drop for HeldWorker<'_> {
    fn drop(&mut self) {
        self.release_and_reap();
    }
}

fn poll_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn job_yaml(name: &str, max_active_runs: u32) -> String {
    format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  max_active_runs: {max_active_runs}
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
    )
}

fn seed_catalog_job(runtime: &OrbitRuntime, name: &str, max_active_runs: u32) {
    let jobs_dir = runtime.paths().jobs_dir.clone();
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join(format!("{name}.yaml")),
        job_yaml(name, max_active_runs),
    )
    .expect("write catalog job");
}

fn write_job_file(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).expect("create job dir");
    let path = dir.join(format!("{name}.yaml"));
    std::fs::write(&path, contents).expect("write job file");
    path
}

#[test]
fn catalog_submission_persists_a_pending_run_and_returns_before_completion() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &catalog_submission_persists_a_pending_run_and_returns_before_completion,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_ok", 1);
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let invoke = runtime
        .submit_job_run("qa_submit_ok", serde_json::json!({}), Some("test"))
        .expect("submission succeeds");

    assert_eq!(invoke.job_name, "qa_submit_ok");
    assert!(!invoke.queued, "an unclaimed slot must not report queued");
    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_submit_ok".to_string()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1, "submission persists exactly one run");
    assert_eq!(runs[0].run_id, invoke.run_id);
    // The submission returns while the run is still owed to its worker: it
    // claims durability and startup, never the eventual job outcome.
    assert_eq!(runs[0].state, JobRunState::Pending);
}

#[test]
fn submission_reports_queued_when_the_job_is_at_its_active_run_limit() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &submission_reports_queued_when_the_job_is_at_its_active_run_limit,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_queued", 1);
    runtime
        .stores()
        .jobs()
        .insert_job_run("qa_submit_queued", 1, Utc::now(), None, None)
        .expect("insert the run already holding the slot");
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let invoke = runtime
        .submit_job_run("qa_submit_queued", serde_json::json!({}), Some("test"))
        .expect("a queued submission still succeeds");

    assert!(
        invoke.queued,
        "the second run of a max_active_runs=1 job must report queued"
    );
}

#[test]
fn test_submission_requires_an_explicit_worker_override_before_spawning() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &test_submission_requires_an_explicit_worker_override_before_spawning,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_missing_override", 1);

    let error = runtime
        .submit_job_run(
            "qa_submit_missing_override",
            serde_json::json!({}),
            Some("test"),
        )
        .expect_err("test builds must not re-exec the libtest binary as a pipeline worker");
    assert!(
        error
            .to_string()
            .contains("requires an explicit worker command override"),
        "the submission must fail closed before spawning a worker: {error}"
    );

    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_submit_missing_override".to_string()),
            ..Default::default()
        })
        .expect("list submitted runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, JobRunState::Interrupted);
    assert_eq!(runs[0].pid, None, "no worker process may have started");
}

/// A worker that cannot start is the *submission's* failure: the caller is
/// told, and the run it already persisted is terminalized rather than left
/// pending forever.
#[test]
fn worker_startup_failure_fails_the_submission_and_terminalizes_the_run() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &worker_startup_failure_fails_the_submission_and_terminalizes_the_run,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_no_worker", 1);
    let _worker = WorkerOverride::missing_program();

    let error = runtime
        .submit_job_run("qa_submit_no_worker", serde_json::json!({}), Some("test"))
        .expect_err("a worker that cannot spawn must fail the submission");
    assert!(
        error.to_string().contains("spawn pipeline worker"),
        "the failure must name the worker startup: {error}"
    );

    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_submit_no_worker".to_string()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].state,
        JobRunState::Interrupted,
        "a run whose worker never started must not stay pending"
    );
}

#[test]
fn strict_config_refuses_unavailable_scope_without_starting_a_worker() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &strict_config_refuses_unavailable_scope_without_starting_a_worker,
    )) {
        return;
    }
    let root = TempDir::new().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("global");
    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::write(
        global_root.join("config.toml"),
        "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"test\"\ntask_prefix = \"TST\"\nworker_containment_strict = true\n",
    )
    .expect("config");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .expect("runtime with strict config");
    seed_catalog_job(&runtime, "qa_strict_scope", 1);
    let marker = root.path().join("worker-started");
    let _worker = WorkerOverride::shell(&format!("touch {}", marker.display()));
    let _manager = TestScopeAvailability::unavailable("no systemd user manager");

    let error = runtime
        .submit_job_run("qa_strict_scope", serde_json::json!({}), Some("test"))
        .expect_err("strict config must refuse unavailable containment");
    assert!(
        matches!(error, OrbitError::WorkerContainmentUnavailable { .. }),
        "{error:?}"
    );
    assert!(!marker.exists(), "uncontained worker must not start");
    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("qa_strict_scope".into()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, JobRunState::Interrupted);
    assert_eq!(runs[0].pid, None);
    assert_eq!(
        runs[0]
            .steps
            .last()
            .and_then(|step| step.error_code.as_deref()),
        Some("worker_containment_unavailable")
    );
    let reason = runs[0]
        .steps
        .last()
        .and_then(|step| step.error_message.as_deref())
        .expect("diagnostic");
    assert!(reason.contains("no systemd user manager"), "{reason}");
    assert!(
        reason.contains("drop --strict-worker-containment"),
        "{reason}"
    );
}

#[test]
fn strict_cli_override_refuses_unavailable_auto_coordinator() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &strict_cli_override_refuses_unavailable_auto_coordinator,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    write_job_file(
        &runtime.paths().global_dir.join("resources/jobs"),
        "workspace_auto_pipeline",
        &job_yaml("workspace_auto_pipeline", 1),
    );
    let _worker = WorkerOverride::shell(IDLE_WORKER);
    let _manager = TestScopeAvailability::unavailable("systemd-run missing");
    let error = runtime
        .submit_workspace_auto_run_with_containment(
            None,
            None,
            crate::CompletionPolicy::Review,
            &[],
            &orbit_config::ComplexityCrewPools::default(),
            None,
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
            true,
        )
        .expect_err("CLI strict override must refuse unavailable containment");
    assert!(
        matches!(error, OrbitError::WorkerContainmentUnavailable { .. }),
        "{error:?}"
    );
    let runs = runtime
        .list_job_runs(JobRunListParams {
            job_id: Some("workspace_auto_pipeline".into()),
            ..Default::default()
        })
        .expect("list runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, JobRunState::Interrupted);
    assert_eq!(runs[0].pid, None);
    assert_eq!(
        runs[0].input.as_ref().expect("run input")["__worker_containment_strict"],
        true
    );
    assert_eq!(
        runs[0]
            .steps
            .last()
            .and_then(|step| step.error_code.as_deref()),
        Some("worker_containment_unavailable")
    );
}

#[test]
fn strict_cli_override_rejects_disabled_containment_before_run_creation() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &strict_cli_override_rejects_disabled_containment_before_run_creation,
    )) {
        return;
    }
    let root = TempDir::new().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("global");
    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::write(
        global_root.join("config.toml"),
        "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"test\"\ntask_prefix = \"TST\"\nworker_containment = false\n",
    )
    .expect("config");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
        .expect("runtime with disabled containment");
    let error = runtime
        .submit_workspace_auto_run_with_containment(
            None,
            None,
            crate::CompletionPolicy::Review,
            &[],
            &orbit_config::ComplexityCrewPools::default(),
            None,
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
            true,
        )
        .expect_err("strict CLI flag requires enabled containment");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .contains("machine.worker_containment=true")
    );
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("runs")
            .is_empty()
    );
}

/// A run that dies after submission is reported by the waiter, never by the
/// submission — which had already succeeded.
#[test]
fn waiting_surfaces_a_terminal_state_the_submission_could_not_know() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &waiting_surfaces_a_terminal_state_the_submission_could_not_know,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_async_fail", 1);
    let _worker = WorkerOverride::shell("echo 'worker gave up' >&2; exit 23");

    let invoke = runtime
        .submit_job_run("qa_submit_async_fail", serde_json::json!({}), Some("test"))
        .expect("submission succeeds even though the run will not");

    let wait = runtime
        .wait_pipeline_runs(std::slice::from_ref(&invoke.run_id), 30, 1, Some("test"))
        .expect("wait completes");
    let entry = wait
        .results
        .into_iter()
        .find(|entry| entry.run_id == invoke.run_id)
        .expect("wait reports the submitted run");

    assert_eq!(entry.status, "interrupted");
    let detail = entry.error.expect("a failed wait must carry a diagnostic");
    assert!(
        detail.contains("worker log"),
        "the diagnostic must point at the worker log: {detail}"
    );
}

/// A direct path names an unmanaged file. The submitted run must execute the
/// definition that was validated, so the exact YAML is pinned next to the run
/// before submission returns — later edits and deletions cannot reach it.
#[test]
fn direct_path_submission_pins_the_validated_definition_against_later_edits() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &direct_path_submission_pins_the_validated_definition_against_later_edits,
    )) {
        return;
    }
    let (root, runtime) = test_runtime();
    let source = write_job_file(
        &root.path().join("loose"),
        "qa_direct",
        &job_yaml("qa_direct", 1),
    );
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let invoke = runtime
        .submit_job_run(
            &source.display().to_string(),
            serde_json::json!({}),
            Some("test"),
        )
        .expect("direct-path submission succeeds");

    let snapshot = run_definition_snapshot_path(&runtime.paths().job_runs_dir, &invoke.run_id)
        .expect("snapshot path validation");
    let pinned = std::fs::read_to_string(&snapshot).expect("definition snapshot is durable");
    assert_eq!(pinned, job_yaml("qa_direct", 1));

    // Mutate, then delete, the file the operator named.
    std::fs::write(&source, job_yaml("qa_direct_rewritten", 9)).expect("rewrite source");
    std::fs::remove_file(&source).expect("delete source");

    let run = runtime.show_job_run(&invoke.run_id).expect("run persisted");
    let (resolved_path, spec) = runtime
        .resolve_run_definition(&run)
        .expect("the worker still resolves a definition");
    assert_eq!(resolved_path, snapshot);
    assert_eq!(
        spec.max_active_runs, 1,
        "the pinned definition is unchanged"
    );
    assert_eq!(spec.steps.len(), 1);
    assert_eq!(spec.steps[0].id, "nap");
}

/// Direct-path validation runs in the submitting process, so a definition the
/// worker could not finish is refused before any run exists to inspect.
#[test]
fn direct_path_submission_refuses_a_retired_declaration_before_persisting_a_run() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &direct_path_submission_refuses_a_retired_declaration_before_persisting_a_run,
    )) {
        return;
    }
    let (root, runtime) = test_runtime();
    let source = write_job_file(
        &root.path().join("loose"),
        "qa_direct_retired",
        r#"schemaVersion: 2
kind: Job
metadata:
  name: qa_direct_retired
spec:
  state: enabled
  kind: workflow
  steps:
    - id: assess
      spec:
        type: agent_loop
        instruction: assess
        tools: []
      session: assessor
"#,
    );
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let error = runtime
        .submit_job_run(
            &source.display().to_string(),
            serde_json::json!({}),
            Some("test"),
        )
        .expect_err("a retired `session:` binding must be refused");
    assert!(
        matches!(error, OrbitError::InvalidInput(ref message) if message.contains("CLI agent path")),
        "the refusal must carry the migration: {error:?}"
    );
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("list runs")
            .is_empty(),
        "a refused submission persists no run"
    );
}

/// Both spellings resolve through the same core entry point, so a subroutine
/// is refused identically whichever one an operator typed.
#[test]
fn submission_refuses_a_subroutine_job() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &submission_refuses_a_subroutine_job,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let jobs_dir = runtime.paths().jobs_dir.clone();
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join("qa_subroutine.yaml"),
        job_yaml("qa_subroutine", 1).replace("kind: workflow", "kind: subroutine"),
    )
    .expect("write subroutine job");
    let _worker = WorkerOverride::shell(IDLE_WORKER);

    let error = runtime
        .submit_job_run("qa_subroutine", serde_json::json!({}), Some("test"))
        .expect_err("a subroutine cannot be run directly");
    assert!(
        error.to_string().contains("kind: subroutine"),
        "the refusal must name the declared kind: {error}"
    );
}

/// Guard against the submission quietly waiting: the caller returns while the
/// worker is still alive. The worker cannot exit until the test releases it,
/// so a submission that waited for it would only return after the worker's
/// own bounded self-release, with its exit already marked.
#[test]
fn submission_returns_while_its_worker_is_still_running() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &submission_returns_while_its_worker_is_still_running,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_catalog_job(&runtime, "qa_submit_nonblocking", 1);
    let worker = HeldWorker::install(&runtime, "qa_submit_nonblocking");

    runtime
        .submit_job_run("qa_submit_nonblocking", serde_json::json!({}), Some("test"))
        .expect("submission succeeds");

    assert!(
        !worker.exited(),
        "submission must return before the worker it started exits"
    );
    assert!(
        worker.wait_until_started(),
        "the submitted worker must start"
    );
    assert!(
        !worker.exited(),
        "the submitted worker must stay alive until released"
    );
    assert!(
        worker.release_and_reap(),
        "the released worker must exit and be reaped"
    );
    assert!(worker.exited(), "the released worker ran to its exit");
}

#[test]
fn auto_complexity_pool_is_captured_at_submission_and_retained_by_real_resume() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &auto_complexity_pool_is_captured_at_submission_and_retained_by_real_resume,
    )) {
        return;
    }
    use crate::application::job::pipeline::{ChildPipelineAdmission, ChildSubmission};
    use crate::application::task::TaskAddParams;
    use orbit_config::ComplexityCrewPools;
    use orbit_types::task::TaskComplexity;
    use serde_json::json;

    let (_root, runtime) = test_runtime();
    for job in [
        "workspace_auto_pipeline",
        "task_auto_pipeline",
        "task_gate_pipeline",
    ] {
        write_job_file(
            &runtime.paths().global_dir.join("resources/jobs"),
            job,
            &job_yaml(job, 10),
        );
    }
    let _worker = WorkerOverride::shell("sleep 5");
    // A record with no crew of its own, so the coordinator's pool decides:
    // [ORB-12717] assigns a crew to anything created through `add_task`.
    let task = runtime
        .add_crew_less_task_for_tests(TaskAddParams {
            title: "Automatic crew persistence".into(),
            description: "Admission and resume preserve crew evidence".into(),
            plan: "Validate persisted input".into(),
            complexity: TaskComplexity::Medium,
            status: Some(orbit_types::task::TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task");
    let parent = runtime
        .submit_workspace_auto_run(
            None,
            None,
            crate::CompletionPolicy::Review,
            &[],
            &ComplexityCrewPools {
                medium: Some(vec!["grok".into(), "terra".into()]),
                ..Default::default()
            },
            None,
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect("submit coordinator");
    let parent_run = runtime
        .get_job_run_backend(&parent.run_id)
        .expect("read coordinator")
        .expect("coordinator");
    assert_eq!(
        parent_run.input.as_ref().expect("input")["auto_crew_pools"]["medium"]["source"],
        "run_input.medium_complexity_crews"
    );
    let child = runtime
        .submit_child_pipeline_run(
            "task_auto_pipeline",
            json!({"task_ids": [task.id]}),
            None,
            None,
            &ChildPipelineAdmission {
                parent_run_id: parent.run_id,
                parent_step_id: Some("ship_leaves".into()),
                action: "invoke_detached".into(),
                blocking: false,
            },
        )
        .expect("admit child");
    let ChildSubmission::Submitted(child) = child else {
        panic!("child admitted");
    };
    let run = runtime
        .get_job_run_backend(&child.run_id)
        .expect("read child")
        .expect("child");
    let input = run.input.expect("child input");
    assert!(["grok", "terra"].contains(&input["crew"].as_str().expect("crew")));
    assert_eq!(
        input["crew_selection"]["source"],
        "run_input.medium_complexity_crews"
    );
    assert!(input.get("allowed_crews").is_none());
    let nested = runtime
        .submit_child_pipeline_run(
            "task_gate_pipeline",
            json!({"task_ids": [task.id]}),
            None,
            None,
            &ChildPipelineAdmission {
                parent_run_id: child.run_id.clone(),
                parent_step_id: Some("gate".into()),
                action: "invoke_and_wait".into(),
                blocking: true,
            },
        )
        .expect("admit same-task pipeline");
    let ChildSubmission::Submitted(nested) = nested else {
        panic!("nested admitted");
    };
    let nested_input = runtime
        .get_job_run_backend(&nested.run_id)
        .expect("read nested")
        .expect("nested")
        .input
        .expect("input");
    assert_eq!(nested_input["crew_selection"], input["crew_selection"]);
    for run_id in [&nested.run_id, &child.run_id] {
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(run_id, Utc::now(), std::process::id())
            .expect("start fixture before recording failure");
        runtime
            .finalize_job_run_with_reservation_cleanup(
                run_id,
                JobRunState::Failed,
                Utc::now(),
                Some(1),
                orbit_store::contracts::TaskReservationReleaseReason::RunTerminal,
            )
            .expect("finish fixture run");
    }
    let resumed = runtime
        .submit_resume_run(&child.run_id, None, None)
        .expect("resume child");
    let resumed_input = runtime
        .get_job_run_backend(&resumed.run_id)
        .expect("read resume")
        .expect("resumed")
        .input
        .expect("input");
    assert_eq!(resumed_input["crew"], input["crew"]);
    assert_eq!(resumed_input["crew_selection"], input["crew_selection"]);
}
