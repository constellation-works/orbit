//! [ORB-14266] A local run its provider failed holds its task in the backlog
//! instead of blocking it, and the next admission draws another crew or
//! defers.
//!
//! Each test fails a real run against the composed runtime: the step's
//! failure is recorded, the run is finalized through Core's ordinary cleanup,
//! and admission is read through `list_backlog_tasks`, the action every local
//! drain runs. A run-scoped crew allowlist exposes the crews the draw would
//! choose from: a task is admitted under `allowed_crews` exactly when a crew
//! it may be drawn onto is in the list, and a `crew_not_allowed` exclusion
//! names those crews.
//!
//! Only the end-to-end refusal fixture requires a Unix shell; the hold and
//! admission tests compile on every platform.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;

use chrono::{Duration, Utc};
#[cfg(unix)]
use orbit_common::OrbitError;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_engine::WORKFLOW_RUN_FAILED_EVENT;
#[cfg(unix)]
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
#[cfg(unix)]
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, ResolvedCliExecutor,
    V2AuditWriter, execute_job_with_resume,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{JobRunStepParams, JobRunStoreBackend};
use orbit_tools::ToolContext;
#[cfg(unix)]
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, AgentLoopSpec, JobV2, OnDenial, Provider,
};
use orbit_types::workflow::{
    JobRunState, JobTargetType, PROVIDER_FAILURE_HOLD_EVENT, PipelineState, ProviderFailureClass,
    ProviderFailureHold, provider_failure_text,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

/// Two Codex crews in the task's complexity pool and a Claude default.
const MIXED_CREWS: &str = r#"[workflow]
default_crew = "opus"
medium_complexity_crews = ["sol", "luna"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"

[crews.opus]
provider = "claude"
model = "opus-model"
"#;

/// Every crew runs on Codex.
const CODEX_ONLY: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol", "luna"]

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.luna]
provider = "codex"
model = "luna-model"
"#;

/// What Codex wrote when its content filter ended a review of sandbox code
/// (run jrun-20261006-0104-c24).
const CONTENT_FILTER: &str = "This content was flagged for possible cybersecurity risk. If this \
     seems wrong, try rephrasing your request. To get authorized for security work, join the \
     Trusted Access for Cyber program: https://chatgpt.com/cyber";

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn fixture(config: &str) -> Fixture {
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    Fixture {
        _root: root,
        runtime,
        repo,
        jobs,
    }
}

impl Fixture {
    /// An admissible backlog task that runs as `crew`.
    fn task(&self, crew: &str) -> String {
        std::fs::write(self.repo.join("feature.txt"), "feature\n").unwrap();
        self.runtime
            .add_task(TaskAddParams {
                title: "Provider failure fixture".to_string(),
                description: "A task whose run fails on its provider.".to_string(),
                acceptance_criteria: vec!["Delivered.".to_string()],
                plan: "1. Deliver it.".to_string(),
                context_files: vec!["file:feature.txt".to_string()],
                complexity: TaskComplexity::Medium,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Backlog),
                crew: Some(crew.to_string()),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// A running local pipeline that admitted `task` on `crew`.
    fn admit(&self, task: &str, crew: &str) -> String {
        let input = json!({ "task_ids": [task], "crew": crew });
        let run = self
            .jobs
            .insert_job_run(
                "task_local_pipeline",
                1,
                Utc::now(),
                Some(input.clone()),
                None,
            )
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        self.jobs
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, input),
            )
            .unwrap();
        self.runtime
            .apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(run.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        run.run_id
    }

    /// Record the run's failed step and finalize it as `failed`, as its
    /// worker does.
    fn fail(&self, run: &str, message: &str) {
        let now = Utc::now();
        self.runtime
            .complete_job_run_step(
                run,
                &JobRunStepParams {
                    step_index: 1,
                    target_type: JobTargetType::Activity,
                    target_id: "implement_one".to_string(),
                    started_at: now,
                    finished_at: now,
                    duration_ms: Some(1),
                    exit_code: Some(1),
                    agent_response_json: None,
                    state: JobRunState::Failed,
                    error_code: Some("STEP_FAILED".to_string()),
                    error_message: Some(message.to_string()),
                },
            )
            .unwrap();
        self.runtime
            .finalize_job_run(run, JobRunState::Failed, now, Some(1))
            .unwrap();
    }

    fn status(&self, task: &str) -> TaskStatus {
        self.runtime.get_task(task).unwrap().status
    }

    /// The hold the task's last history entry records, with that entry's
    /// event and status.
    fn last_hold(&self, task: &str) -> ProviderFailureHold {
        let history = self.runtime.get_task_history(task).unwrap();
        let entry = history.last().expect("history");
        assert_eq!(entry.event, PROVIDER_FAILURE_HOLD_EVENT, "{entry:?}");
        assert_eq!(entry.to_status, Some(TaskStatus::Backlog), "{entry:?}");
        ProviderFailureHold::from_text(entry.note.as_deref().unwrap())
            .unwrap_or_else(|| panic!("the note carries the hold: {entry:?}"))
    }

    fn backlog(&self, allowed_crews: &[&str]) -> Value {
        self.runtime
            .run_deterministic(
                "list_backlog_tasks",
                &json!({}),
                &json!({ "allowed_crews": allowed_crews }),
                ToolContext::default(),
            )
            .unwrap()
    }

    fn admitted(&self, task: &str, allowed_crews: &[&str]) -> bool {
        self.backlog(allowed_crews)["task_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == task)
    }

    fn exclusion(&self, task: &str, allowed_crews: &[&str]) -> Value {
        self.backlog(allowed_crews)["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == task)
            .cloned()
            .unwrap_or_else(|| panic!("{task} is excluded under {allowed_crews:?}"))
    }
}

fn step_failure(class: ProviderFailureClass, detail: &str) -> String {
    format!(
        "step `implement_one`: {}",
        provider_failure_text(class, "codex", detail)
    )
}

/// Capacity and an unusable provider hold the task in the backlog with the
/// run's crew excluded until a backoff passes; another pool member takes it
/// meanwhile, and a second failure narrows the draw further. Any other
/// failure still blocks.
#[test]
fn capacity_and_unavailability_hold_the_task_with_the_run_crew_excluded() {
    if !isolated(
        "provider_failure_hold::capacity_and_unavailability_hold_the_task_with_the_run_crew_excluded",
    ) {
        return;
    }
    for (class, detail) in [
        (
            ProviderFailureClass::Capacity,
            "cli subprocess exited with code 1: codex provider reported the selected model at \
             capacity: Selected model is at capacity. Please try a different model.",
        ),
        (
            ProviderFailureClass::Unavailable,
            "codex provider authentication failure (HTTP 401): token expired",
        ),
    ] {
        let fx = fixture(MIXED_CREWS);
        let task = fx.task("sol");
        let run = fx.admit(&task, "sol");
        let before = Utc::now();
        fx.fail(&run, &step_failure(class, detail));

        assert_eq!(fx.status(&task), TaskStatus::Backlog, "{class:?}");
        let hold = fx.last_hold(&task);
        assert_eq!(hold.class, class);
        assert_eq!(hold.provider.as_deref(), Some("codex"));
        assert_eq!(
            hold.excluded_crews,
            ["sol"],
            "only the run's crew: {hold:?}"
        );
        assert_eq!(hold.run_id, run);
        assert!(
            hold.not_before > before + Duration::minutes(10),
            "a not-before backoff: {hold:?}"
        );
        assert!(
            fx.runtime
                .get_task_history(&task)
                .unwrap()
                .iter()
                .all(|entry| entry.event != WORKFLOW_RUN_FAILED_EVENT),
            "the task is not blocked"
        );

        // Before not-before the task is admitted, drawn onto the pool's
        // other crew rather than the one that failed.
        assert!(fx.admitted(&task, &[]), "another crew may take it");
        assert!(fx.admitted(&task, &["luna"]));
        let excluded = fx.exclusion(&task, &["sol"]);
        assert_eq!(excluded["reason"], "crew_not_allowed");
        assert_eq!(excluded["crew"], "luna", "the held draw: {excluded}");

        // That crew fails too: both stay excluded, for longer, and the draw
        // falls back to the workspace default.
        let second = fx.admit(&task, "luna");
        fx.fail(&second, &step_failure(class, detail));
        let narrowed = fx.last_hold(&task);
        assert_eq!(narrowed.excluded_crews, ["luna", "sol"], "{narrowed:?}");
        assert!(narrowed.not_before > hold.not_before, "{narrowed:?}");
        assert!(fx.admitted(&task, &["opus"]));
        assert_eq!(fx.exclusion(&task, &["sol", "luna"])["crew"], "opus");
    }

    // Any other failure still blocks the task.
    let fx = fixture(MIXED_CREWS);
    let task = fx.task("sol");
    let run = fx.admit(&task, "sol");
    fx.fail(
        &run,
        "step `implement_one`: cli subprocess exited with code 1",
    );
    assert_eq!(fx.status(&task), TaskStatus::Blocked);
    let history = fx.runtime.get_task_history(&task).unwrap();
    assert_eq!(history.last().unwrap().event, WORKFLOW_RUN_FAILED_EVENT);
    assert!(
        history
            .iter()
            .all(|entry| entry.event != PROVIDER_FAILURE_HOLD_EVENT)
    );
}

/// When the hold excludes every crew the task could run as, the drain defers
/// it with the reason and time; once not-before passes it is admitted again.
#[test]
fn a_hold_that_excludes_every_crew_defers_the_task_until_not_before() {
    if !isolated(
        "provider_failure_hold::a_hold_that_excludes_every_crew_defers_the_task_until_not_before",
    ) {
        return;
    }
    let fx = fixture(CODEX_ONLY);
    let task = fx.task("sol");
    let run = fx.admit(&task, "sol");
    fx.fail(
        &run,
        &step_failure(
            ProviderFailureClass::Refusal,
            &format!("cli subprocess exited with code 1: codex provider refused the request: {CONTENT_FILTER}"),
        ),
    );

    let hold = fx.last_hold(&task);
    // The synthesized `system` crew mirrors a Codex crew here, so it is
    // excluded with them.
    assert_eq!(
        hold.excluded_crews,
        ["luna", "sol", "system"],
        "every Codex crew"
    );
    assert!(!fx.admitted(&task, &[]));
    let deferred = fx.exclusion(&task, &[]);
    assert_eq!(deferred["reason"], "provider_backoff", "{deferred}");
    let detail = deferred["detail"].as_str().unwrap();
    assert!(
        detail.contains(&run)
            && detail.contains("provider_refusal")
            && detail.contains(&hold.not_before.to_rfc3339()),
        "the deferral names the run, the failure and the time: {detail}"
    );

    // The same hold, lapsed, no longer withholds anything.
    let lapsed = ProviderFailureHold {
        not_before: Utc::now() - Duration::minutes(1),
        ..hold
    };
    for (status, event, note) in [
        (TaskStatus::InProgress, None, None),
        (
            TaskStatus::Backlog,
            Some(PROVIDER_FAILURE_HOLD_EVENT.to_string()),
            Some(lapsed.text("lapsed fixture hold")),
        ),
    ] {
        fx.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(status),
                    status_event: event,
                    status_note: note,
                    ..Default::default()
                },
            )
            .unwrap();
    }
    assert!(fx.admitted(&task, &["sol"]), "the hold has lapsed");
}

/// The incident end to end: a fake Codex emits its content-filter frames, the
/// engine types the failure and skips final recovery, Core holds the task
/// with every Codex crew excluded, and the next admission draws the Claude
/// crew.
#[cfg(unix)]
#[test]
fn a_content_filter_refusal_moves_the_next_admission_to_another_provider() {
    if !isolated(
        "provider_failure_hold::a_content_filter_refusal_moves_the_next_admission_to_another_provider",
    ) {
        return;
    }
    let fx = fixture(MIXED_CREWS);
    let task = fx.task("sol");
    let run = fx.admit(&task, "sol");
    let codex = FakeCodex::new(&fx.repo, &content_filter_frames());
    let host = Pipeline {
        fixture: &fx,
        codex: codex.path.clone(),
        final_recovery_admissions: Mutex::new(0),
    };
    let audit = V2AuditWriter::with_disk_sinks(
        &fx.repo.join("audit"),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_fixture",
        &run,
        "fixture",
        Some(&fx.repo),
    )
    .unwrap();
    let outcome = execute_job_with_resume(
        &implementation_job(),
        json!({ "prompt": "implement", "task_ids": [task] }),
        &run,
        audit,
        &host,
        None,
    );
    let message = match &outcome {
        Ok(outcome) => {
            assert!(!outcome.success, "{outcome:?}");
            outcome.message.clone().unwrap_or_default()
        }
        Err(error) => error.to_string(),
    };
    assert!(
        orbit_types::workflow::is_provider_refusal(None, Some(&message)),
        "the content filter is a typed provider refusal: {message}"
    );
    assert_eq!(
        *host.final_recovery_admissions.lock().unwrap(),
        0,
        "final recovery is not dispatched"
    );
    fx.fail(&run, &message);

    assert_eq!(fx.status(&task), TaskStatus::Backlog);
    let hold = fx.last_hold(&task);
    assert_eq!(hold.class, ProviderFailureClass::Refusal);
    assert_eq!(hold.excluded_crews, ["luna", "sol"], "every Codex crew");
    assert!(fx.admitted(&task, &["opus"]), "the Claude crew takes it");
    let excluded = fx.exclusion(&task, &["sol", "luna"]);
    assert_eq!(excluded["reason"], "crew_not_allowed");
    assert_eq!(
        excluded["crew"], "opus",
        "no Codex crew is drawn: {excluded}"
    );
}

#[cfg(unix)]
fn content_filter_frames() -> String {
    format!(
        r#"{{"type":"item.completed","item":{{"id":"item_1","type":"command_execution","command":"rg sandbox","aggregated_output":"","exit_code":0,"status":"completed"}}}}
{{"type":"error","message":"{CONTENT_FILTER}"}}
{{"type":"turn.failed","error":{{"message":"{CONTENT_FILTER}"}}}}"#
    )
}

/// A fake `codex` that prints `stdout` and exits 1.
#[cfg(unix)]
struct FakeCodex {
    path: PathBuf,
}

#[cfg(unix)]
impl FakeCodex {
    fn new(dir: &Path, stdout: &str) -> Self {
        let path = dir.join("codex");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncat > /dev/null\ncat <<'STDOUT'\n{stdout}\nSTDOUT\nexit 1\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { path }
    }
}

/// Scripts the deterministic steps and the provider launcher; everything
/// else is the engine's default host behaviour.
#[cfg(unix)]
struct Pipeline<'a> {
    fixture: &'a Fixture,
    codex: PathBuf,
    final_recovery_admissions: Mutex<usize>,
}

#[cfg(unix)]
impl RuntimeHost for Pipeline<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        _input: &Value,
        _context: ToolContext,
    ) -> Result<Value, DispatchError> {
        Ok(match action {
            "setup" => json!({ "workspace_path": self.fixture.repo, "base_ref": "main" }),
            _ => json!({ "action": action }),
        })
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.codex.to_string_lossy().into_owned(),
            args: Vec::new(),
        })
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        ToolContext::default()
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        _request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        *self.final_recovery_admissions.lock().unwrap() += 1;
        Ok(FinalRecoveryAdmission::Admitted)
    }
}

#[cfg(unix)]
fn deterministic_activity(name: &str) -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": name },
        "spec": { "type": "deterministic", "description": name, "action": name, "config": {} },
    });
    load_activity_asset(&asset.to_string()).unwrap().spec
}

/// `setup → implement_one` on Codex, with step and final recovery and a
/// failure handoff, so a skipped recovery is the executor's own decision.
#[cfg(unix)]
fn implementation_job() -> JobV2 {
    let spec = AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "implement the task".to_string(),
        tools: vec![],
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Codex,
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    };
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "provider_refusal_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [
                { "id": "setup", "spec": { "type": "deterministic", "action": "setup", "config": {} } },
                { "id": "implement_one", "spec": ActivityV2Spec::AgentLoop(spec) },
            ],
        },
    });
    let mut job = load_job_asset(&asset.to_string()).unwrap().spec;
    job.steps[1].recovery_activity = Some("step_fix".to_string());
    job.steps[1].resolved_recovery_activity = Some(deterministic_activity("step_fix"));
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));
    job
}
