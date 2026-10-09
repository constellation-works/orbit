#![cfg(unix)]

//! [ORB-14152] Step recovery's durable decision, end to end.
//!
//! The shipped `step_failure_recovery` activity runs through the CLI runner
//! against a substitute provider that writes, withholds or forges the
//! decision file its input names; the composed runtime allocates that file
//! and reads it back; the engine admits or refuses the one post-recovery
//! attempt of a failing deterministic step. The provider's final response is
//! scripted independently, so each case shows which of the two decides.
//! [ORB-14268] Without a decision, only a change the engine observes in the
//! worktree or validation environment admits the attempt, and an
//! `external_blocker` decision ends the step as a typed blocker that final
//! recovery skips.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, ResolvedActivityTools, ResolvedCliExecutor, RuntimeHost,
    StepRecoveryDecisionRead, StepRecoveryDecisionRequest, StepRecoveryDecisionSlot,
    TaskActivityUpdate, TaskAutomationUpdate, V2AuditWriter, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::task::{ContextWideningStep, ExecutionLocation};
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::activity_job::{
    JobV2, StepRecoveryDecisionRecord, V2AuditEvent, V2AuditEventKind,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

/// [ORB-14822] The host commits a recovery's repair before the retry.
mod repair_commit;

const ORIGINAL_FAILURE: &str = "required validation is red on the candidate";
const RETRY_FAILURE: &str = "required validation is still red";
const TASK: &str = "ORB-1";

/// A frame-only final response: the completion protocol is satisfied and
/// nothing about the decision can be read from it.
const FRAME_ONLY: &str = r#"{"schemaVersion":1,"status":"success","result":null,"error":null}"#;
const SAYS_RECOVERED: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"recovered":true},"error":null}"#;
const SAYS_UNRECOVERED: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"recovered":false},"error":null}"#;
const DECLARES_FAILED: &str = r#"{"schemaVersion":1,"status":"failed","result":{"recovered":true},"error":{"code":"blocked","message":"recovery declared failure"}}"#;

/// Shell that writes a decision bound to this invocation. `$1` is the verdict.
/// `$root` is the assigned worktree the decision path lives under.
const WRITE_BOUND: &str = r#"bound() { printf '{"schema_version":1,"run_id":"%s","failed_step_id":"%s","attempt":%s,"nonce":"%s","decision":"%s","reason":"%s"}' "$run" "$step" "$attempt" "$nonce" "$1" "$2"; }
root=$(dirname "$(dirname "$(dirname "$(dirname "$path")")")")"#;

/// Appends `blocker` to a decision [`WRITE_BOUND`] printed.
const WITH_BLOCKER: &str = r#"sed 's/}$/,"blocker":{"kind":"missing_credentials","evidence":"the push token was revoked"}}/'"#;

struct Fixture {
    _root: TempDir,
    root: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
}

fn git(dir: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(dir).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(dir).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A registered primary checkout; each case runs in its own linked worktree,
/// as a managed recovery does.
fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "seed"]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    Fixture {
        root: root.path().to_path_buf(),
        repo,
        _root: root,
        runtime,
    }
}

/// One scripted recovery: what the provider writes and answers, and whether
/// the failed step's re-attempt (if any) passes.
struct Case<'a> {
    name: &'a str,
    /// Shell run by the provider with `$path`, `$run`, `$step`, `$attempt`
    /// and `$nonce` from its `recovery_decision` input, after [`WRITE_BOUND`].
    producer: &'a str,
    response: &'a str,
    retry_succeeds: bool,
    read_failure: bool,
    claimed: bool,
    change_validation_env: bool,
    /// Runs on the fresh worktree before the job starts.
    prepare: fn(&Path),
    /// Steps that ran before `validate`, as `(id, recorded output)`: a
    /// `git_commit` outcome makes `validate` a step after the commit. A
    /// `"HEAD"` string in an output is replaced with the worktree's head.
    before: Vec<(&'a str, Value)>,
}

impl Default for Case<'_> {
    fn default() -> Self {
        Self {
            name: "case",
            producer: "",
            response: FRAME_ONLY,
            retry_succeeds: true,
            read_failure: false,
            claimed: false,
            change_validation_env: false,
            prepare: |_| {},
            before: Vec::new(),
        }
    }
}

struct Observed {
    deliveries: usize,
    /// The job's terminal hooks the engine dispatched, with their inputs.
    hooks: Vec<(String, Value)>,
    /// Each final-recovery event's outcome and detail.
    final_recovery: Vec<(String, Option<String>)>,
    result: Result<(bool, Option<String>), String>,
    attempted: StepRecoveryDecisionAttempt,
    post_recovery: Vec<String>,
    envelopes: Vec<Value>,
    task_writes: usize,
    projection: orbit_core::runtime::audit::run::RunRecoveryAttempts,
    /// The worktree's HEAD and `git status --porcelain` at each `validate`
    /// attempt.
    attempts_saw: Vec<(String, String)>,
    worktree: PathBuf,
}

/// The `step.recovery_attempted` event's activity completion, failure phase
/// and decision.
#[derive(Debug)]
struct StepRecoveryDecisionAttempt {
    recovery_succeeded: bool,
    failure_phase: Option<String>,
    error_message: Option<String>,
    decision: Option<StepRecoveryDecisionRecord>,
}

impl Observed {
    fn decision(&self) -> &StepRecoveryDecisionRecord {
        self.attempted
            .decision
            .as_ref()
            .unwrap_or_else(|| panic!("no decision was recorded: {:?}", self.attempted))
    }

    fn failed_with_original(&self) -> bool {
        match &self.result {
            Ok((false, message)) => message
                .as_deref()
                .is_some_and(|m| m.contains(ORIGINAL_FAILURE)),
            Err(message) => {
                message.contains(ORIGINAL_FAILURE) && !message.contains("post-recovery")
            }
            Ok((true, _)) => false,
        }
    }
}

/// Script only the failing step and the provider binary; slot allocation and
/// read-back are the composed runtime's.
struct RecoveryHost<'a> {
    runtime: &'a OrbitRuntime,
    provider: PathBuf,
    retry_succeeds: bool,
    read_failure: bool,
    claimed: bool,
    change_validation_env: bool,
    deliveries: AtomicUsize,
    hooks: Mutex<Vec<(String, Value)>>,
    task_writes: AtomicUsize,
    slots: Mutex<Vec<StepRecoveryDecisionSlot>>,
    validation_env_marker: PathBuf,
    attempts_saw: Mutex<Vec<(String, String)>>,
}

impl RecoveryHost<'_> {
    fn task_write<T>(&self, what: &str) -> Result<T, OrbitError> {
        self.task_writes.fetch_add(1, Ordering::SeqCst);
        Err(OrbitError::Execution(format!(
            "{what}: recovery must not write task state"
        )))
    }
}

impl RuntimeHost for RecoveryHost<'_> {
    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        RuntimeHost::final_recovery_log_tail(self.runtime, run_id)
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        if action == "checkpoint" {
            // `HEAD` in a recorded output names the worktree's live head.
            let head = git_stdout(
                Path::new(input["workspace_path"].as_str().unwrap()),
                &["rev-parse", "HEAD"],
            );
            let output = input["output"]
                .to_string()
                .replace("\"HEAD\"", &format!("\"{head}\""));
            return Ok(serde_json::from_str(&output).unwrap());
        }
        if action != "deliver" {
            self.hooks
                .lock()
                .unwrap()
                .push((action.to_string(), input.clone()));
            return Ok(json!({}));
        }
        let worktree = Path::new(input["workspace_path"].as_str().unwrap());
        self.attempts_saw.lock().unwrap().push((
            git_stdout(worktree, &["rev-parse", "HEAD"]),
            git_stdout(worktree, &["status", "--porcelain"]),
        ));
        let call = self.deliveries.fetch_add(1, Ordering::SeqCst);
        if call > 0 && self.retry_succeeds {
            return Ok(json!({"delivered": true}));
        }
        Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: if call == 0 {
                ORIGINAL_FAILURE
            } else {
                RETRY_FAILURE
            }
            .to_string(),
        })
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.provider.display().to_string(),
            args: Vec::new(),
        })
    }

    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        let mut environment = self.runtime.validation_environment();
        if self.change_validation_env && self.validation_env_marker.exists() {
            if let Some((_, value)) = environment
                .env
                .iter_mut()
                .find(|(name, _)| name == "CARGO_HOME")
            {
                *value = "/changed/cargo-home".to_string();
            } else {
                environment
                    .env
                    .push(("CARGO_HOME".to_string(), "/changed/cargo-home".to_string()));
            }
        }
        environment
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        self.runtime
            .tool_context_for_activity(run_id, fs_profile, fs_audit, proc_allowed_programs)
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("sol".to_string())
    }

    fn agent_crew_config_for_input(
        &self,
        _input: &Value,
    ) -> Result<Option<orbit_engine::CrewConfig>, DispatchError> {
        Ok(None)
    }

    /// The runtime's registry, without task-store lookups: a follower holds
    /// no copy of the owner's task.
    fn resolve_activity_tool_denials(
        &self,
        _task_ids: &[String],
        activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        self.runtime
            .resolve_activity_tool_denials(&[], activity, disallow_list)
    }

    fn worker_invocation(&self) -> Option<WorkerInvocation> {
        self.claimed.then(|| WorkerInvocation {
            owner_machine_id: "hm_owner".to_string(),
            owner_workspace_id: "ws_owner".to_string(),
            owner_destination: "owner".to_string(),
            task_id: TASK.to_string(),
            claim_id: "claim-1".to_string(),
            execution: ExecutionLocation {
                machine_id: "hm_follower".to_string(),
                machine_name: None,
            },
            bound_run_id: "leaf".to_string(),
        })
    }

    /// A follower's drain tracks the worker it launched.
    fn register_worker_process(&self, _pid: u32) -> Result<(), OrbitError> {
        Ok(())
    }

    fn register_worker_pid_namespace(&self, _pid: u32) -> Result<(), OrbitError> {
        Ok(())
    }

    fn allocate_step_recovery_decision(
        &self,
        request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        let slot = self.runtime.allocate_step_recovery_decision(request)?;
        self.slots.lock().unwrap().extend(slot.clone());
        Ok(slot)
    }

    fn read_step_recovery_decision(
        &self,
        slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        if self.read_failure {
            return Err(OrbitError::Io("injected decision read failure".to_string()));
        }
        self.runtime.read_step_recovery_decision(slot)
    }

    fn update_task_from_activity(
        &self,
        _task_id: &str,
        _update: TaskActivityUpdate,
    ) -> Result<orbit_types::task::Task, OrbitError> {
        self.task_write("update_task_from_activity")
    }

    fn apply_task_automation_update(
        &self,
        _task_id: &str,
        _update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        self.task_write("apply_task_automation_update")
    }

    fn widen_task_context_files(
        &self,
        _task_id: &str,
        _run_id: &str,
        _step: ContextWideningStep,
        _activity: &str,
        _paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        self.task_write("widen_task_context_files")
    }
}

/// The failing step under the shipped recovery activity, in a job with a
/// final recovery and a failure activity scripted as deterministic actions.
fn job(worktree: &Path, before: &[(&str, Value)]) -> JobV2 {
    let mut steps = before
        .iter()
        .map(|(id, output)| {
            json!({
                "id": id,
                "spec": {"type": "deterministic", "action": "checkpoint", "config": {}},
                "default_input": {"output": output, "workspace_path": worktree},
            })
        })
        .collect::<Vec<_>>();
    steps.push(json!({
        "id": "validate",
        "recovery_activity": "step_failure_recovery",
        "spec": {"type": "deterministic", "action": "deliver", "config": {}},
        "default_input": {
            "task_id": TASK,
            "workspace_path": worktree,
            "repo_root": worktree,
        },
    }));
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "step_recovery_fixture"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "failure_activity": "preserve_candidate",
                "final_recovery_activity": "final_look",
                "steps": steps,
            },
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    let shipped = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities/step_failure_recovery.yaml"),
    )
    .unwrap();
    let mut catalog = V2ActivityCatalog::new();
    catalog.insert(
        "step_failure_recovery",
        load_activity_asset(&shipped).unwrap().spec,
    );
    for hook in ["preserve_candidate", "final_look"] {
        let asset = json!({
            "schemaVersion": 2,
            "kind": "Activity",
            "metadata": {"name": hook},
            "spec": {
                "type": "deterministic",
                "description": hook,
                "action": hook,
                "config": {},
            },
        });
        catalog.insert(hook, load_activity_asset(&asset.to_string()).unwrap().spec);
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    job
}

/// A substitute `claude` that records its envelope, runs the case's producer
/// against the `recovery_decision` it was handed, and prints the case's
/// final response.
fn provider(dir: &Path, case: &Case<'_>) -> PathBuf {
    let program = dir.join("claude");
    let envelopes = dir.join("envelopes");
    std::fs::create_dir_all(&envelopes).unwrap();
    std::fs::write(
        &program,
        format!(
            r#"#!/bin/sh
input=$(cat)
printf '%s' "$input" > '{envelopes}/'$$.json
slot=$(printf '%s' "$input" | grep -o '"recovery_decision":{{[^}}]*}}' | head -n 1)
field() {{ printf '%s' "$slot" | sed -n "s/.*\"$1\":\"\{{0,1\}}\([^\",}}]*\).*/\1/p"; }}
path=$(field path); run=$(field run_id); step=$(field failed_step_id)
attempt=$(field attempt); nonce=$(field nonce)
{WRITE_BOUND}
{producer}
cat <<'RESPONSE'
{response}
RESPONSE
"#,
            envelopes = envelopes.display(),
            producer = case.producer,
            response = case.response,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

fn run(fixture: &Fixture, case: &Case<'_>) -> Observed {
    let dir = fixture.root.join(case.name);
    let worktree = dir.join("worktree");
    std::fs::create_dir_all(&dir).unwrap();
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            case.name,
            &worktree.display().to_string(),
        ],
    );
    (case.prepare)(&worktree);
    let host = RecoveryHost {
        runtime: &fixture.runtime,
        provider: provider(&dir, case),
        retry_succeeds: case.retry_succeeds,
        read_failure: case.read_failure,
        claimed: case.claimed,
        change_validation_env: case.change_validation_env,
        deliveries: AtomicUsize::new(0),
        hooks: Mutex::new(Vec::new()),
        task_writes: AtomicUsize::new(0),
        slots: Mutex::new(Vec::new()),
        validation_env_marker: worktree.join(".orbit/tmp/validation-env-changed"),
        attempts_saw: Mutex::new(Vec::new()),
    };
    let run_id = format!("run-{}", case.name);
    let audit = V2AuditWriter::with_disk_sinks(
        &dir.join("audit"),
        fixture.runtime.v2_audit_store().unwrap(),
        fixture.runtime.workspace_id().unwrap(),
        &run_id,
        "fixture",
        Some(&worktree),
    )
    .unwrap();
    let result = execute_job_with_resume(
        &job(&worktree, &case.before),
        json!({"task_id": TASK}),
        &run_id,
        audit.clone(),
        &host,
        None,
    )
    .map(|outcome| (outcome.success, outcome.message))
    .map_err(|error| error.to_string());
    let events: Vec<V2AuditEvent> = audit.events_snapshot().unwrap();
    let attempted = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::StepRecoveryAttempted {
                recovery_succeeded,
                failure_phase,
                error_message,
                decision,
                ..
            } => Some(StepRecoveryDecisionAttempt {
                recovery_succeeded: *recovery_succeeded,
                failure_phase: failure_phase.clone(),
                error_message: error_message.clone(),
                decision: decision.clone(),
            }),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{}: recovery was not attempted: {result:?}", case.name));
    let post_recovery = events
        .iter()
        .filter_map(|event| match &event.kind {
            V2AuditEventKind::StepPostRecoveryAttempt { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    let final_recovery = events
        .iter()
        .filter_map(|event| match &event.kind {
            V2AuditEventKind::FinalRecoveryAttempted {
                outcome, detail, ..
            } => Some((outcome.clone(), detail.clone())),
            _ => None,
        })
        .collect();
    let mut envelopes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("envelopes")) {
        for entry in entries {
            let prompt = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            let (_, envelope) = prompt.split_once("Execution envelope:\n").unwrap();
            envelopes.push(
                serde_json::Deserializer::from_str(envelope)
                    .into_iter::<Value>()
                    .next()
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    for slot in host.slots.lock().unwrap().iter() {
        assert!(
            slot.path
                .starts_with(worktree.canonicalize().unwrap().join(".orbit/tmp")),
            "{}: the slot is run-local scratch of the assigned worktree: {}",
            case.name,
            slot.path.display()
        );
    }
    Observed {
        deliveries: host.deliveries.load(Ordering::SeqCst),
        hooks: std::mem::take(&mut *host.hooks.lock().unwrap()),
        final_recovery,
        result,
        attempted,
        post_recovery,
        envelopes,
        task_writes: host.task_writes.load(Ordering::SeqCst),
        projection: fixture
            .runtime
            .collect_run_recovery_attempts(&run_id)
            .unwrap(),
        attempts_saw: std::mem::take(&mut *host.attempts_saw.lock().unwrap()),
        worktree,
    }
}

fn assert_refused(observed: &Observed, case: &str, status: &str, detail: &str) {
    assert!(
        observed.attempted.recovery_succeeded,
        "{case}: the activity completed"
    );
    let decision = observed.decision();
    assert_eq!(decision.status, status, "{case}: {decision:?}");
    assert_eq!(decision.verdict, None, "{case}");
    assert!(!decision.retry_admitted, "{case}");
    assert!(
        decision
            .detail
            .as_deref()
            .is_some_and(|text| text.contains(detail)),
        "{case}: the refusal names its cause `{detail}`: {decision:?}"
    );
    assert_eq!(observed.deliveries, 1, "{case}: no post-recovery attempt");
    assert!(observed.post_recovery.is_empty(), "{case}");
    assert!(
        observed.failed_with_original(),
        "{case}: {:?}",
        observed.result
    );
}

#[test]
fn the_written_decision_controls_the_single_retry_whatever_the_final_response_says() {
    if !isolated(
        "step_recovery::the_written_decision_controls_the_single_retry_whatever_the_final_response_says",
    ) {
        return;
    }
    let fixture = fixture();

    // The response claims recovery; the file says it could not repair.
    let declined = run(
        &fixture,
        &Case {
            name: "declined",
            producer: r#"bound not_recovered "the base is red" > "$path"; printf '\n' >> "$path""#,
            response: SAYS_RECOVERED,
            ..Case::default()
        },
    );
    assert!(declined.attempted.recovery_succeeded);
    let decision = declined.decision();
    assert_eq!(
        decision.status, "verified",
        "a real trailing newline is accepted"
    );
    assert_eq!(decision.verdict.as_deref(), Some("not_recovered"));
    assert!(!decision.retry_admitted);
    assert_eq!(decision.detail.as_deref(), Some("the base is red"));
    assert_eq!(declined.deliveries, 1, "a declined recovery is not retried");
    assert!(declined.post_recovery.is_empty());
    assert!(declined.failed_with_original(), "{:?}", declined.result);
    let input = &declined.envelopes[0]["input"]["recovery_decision"];
    assert_eq!(input["schema_version"], 1);
    assert_eq!(input["run_id"], "run-declined");
    assert_eq!(input["failed_step_id"], "validate");
    assert_eq!(input["attempt"], 1);
    // The operator projection keeps completion and decision apart.
    let attempt = &declined.projection.attempts[0];
    assert_eq!(attempt.outcome, "succeeded");
    assert!(!attempt.retry_admitted);
    let projected = attempt.decision.as_ref().unwrap();
    assert_eq!(projected.status, "verified");
    assert_eq!(projected.verdict.as_deref(), Some("not_recovered"));

    // The response denies recovery; the file asks for the retry, which then
    // passes its own deterministic check.
    let repaired = run(
        &fixture,
        &Case {
            name: "repaired",
            producer: r#"bound retry "formatted the candidate" > "$path""#,
            response: SAYS_UNRECOVERED,
            ..Case::default()
        },
    );
    assert_eq!(repaired.decision().verdict.as_deref(), Some("retry"));
    assert!(repaired.decision().retry_admitted);
    assert_eq!(repaired.deliveries, 2, "exactly one post-recovery attempt");
    assert_eq!(repaired.post_recovery, ["success"]);
    assert_eq!(repaired.result, Ok((true, None)));
    assert!(repaired.projection.attempts[0].retry_admitted);

    // A retry decision admits the attempt, never its success: the re-run
    // step's own check is authoritative, and it still fails here.
    let still_red = run(
        &fixture,
        &Case {
            name: "still_red",
            producer: r#"bound retry "tried a fix" > "$path""#,
            retry_succeeds: false,
            ..Case::default()
        },
    );
    assert_eq!(still_red.deliveries, 2);
    assert_eq!(still_red.post_recovery, ["error"]);
    let error = still_red.result.as_ref().unwrap_err();
    assert!(
        error.contains("post-recovery attempt") && error.contains(RETRY_FAILURE),
        "{error}"
    );

    // Absent result content cannot alter the written decision either way.
    let silent = run(
        &fixture,
        &Case {
            name: "silent",
            producer: r#"bound not_recovered "" > "$path""#,
            response: FRAME_ONLY,
            ..Case::default()
        },
    );
    assert_eq!(silent.decision().verdict.as_deref(), Some("not_recovered"));
    assert_eq!(silent.decision().detail, None, "a blank reason is dropped");
    assert_eq!(silent.deliveries, 1);
}

#[test]
fn without_a_decision_only_an_observed_change_admits_the_retry() {
    if !isolated("step_recovery::without_a_decision_only_an_observed_change_admits_the_retry") {
        return;
    }
    let fixture = fixture();

    // [ORB-14268] Nothing written and nothing changed: the rerun would meet
    // the same cause, so the original failure stands, whatever the response
    // claims.
    let unchanged = run(
        &fixture,
        &Case {
            name: "absent_unchanged",
            response: SAYS_RECOVERED,
            ..Case::default()
        },
    );
    let decision = unchanged.decision();
    assert_eq!(decision.status, "absent");
    assert_eq!(decision.verdict, None);
    assert!(
        !decision.retry_admitted,
        "an unchanged worktree, base and validation environment admit no retry"
    );
    assert!(
        decision
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("no change")),
        "{decision:?}"
    );
    assert_eq!(
        unchanged.envelopes.len(),
        1,
        "the recovery provider runs once"
    );
    assert_eq!(unchanged.deliveries, 1, "no post-recovery attempt");
    assert!(unchanged.post_recovery.is_empty());
    assert!(unchanged.failed_with_original(), "{:?}", unchanged.result);
    let projected = unchanged.projection.attempts[0].decision.as_ref().unwrap();
    assert_eq!(projected.status, "absent");
    assert!(!unchanged.projection.attempts[0].retry_admitted);

    // A repair in the worktree is an observed change: one retry, and
    // `recovered: false` alone suppresses nothing.
    let changed = run(
        &fixture,
        &Case {
            name: "absent_changed",
            producer: r#"printf 'repaired\n' > "$root/repaired.txt""#,
            response: SAYS_UNRECOVERED,
            ..Case::default()
        },
    );
    let decision = changed.decision();
    assert_eq!(decision.status, "absent");
    assert!(decision.retry_admitted, "{decision:?}");
    assert!(
        decision
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("worktree")),
        "the record names the change: {decision:?}"
    );
    assert_eq!(changed.deliveries, 2, "exactly one post-recovery attempt");
    assert_eq!(changed.post_recovery, ["success"]);
    assert_eq!(changed.result, Ok((true, None)));

    // Changing a toolchain locator also changes the environment validation
    // runs with, even when the Git worktree remains untouched.
    let env_changed = run(
        &fixture,
        &Case {
            name: "absent_validation_env_changed",
            producer: r#"touch "$root/.orbit/tmp/validation-env-changed""#,
            change_validation_env: true,
            ..Case::default()
        },
    );
    let decision = env_changed.decision();
    assert_eq!(decision.status, "absent");
    assert!(decision.retry_admitted, "{decision:?}");
    assert!(
        decision
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("validation environment")),
        "the record names the change: {decision:?}"
    );
    assert_eq!(
        env_changed.deliveries, 2,
        "exactly one post-recovery attempt"
    );
    assert_eq!(env_changed.post_recovery, ["success"]);
    assert_eq!(env_changed.result, Ok((true, None)));

    // An unsuccessful activity is never retried, whatever it wrote.
    let declared_failed = run(
        &fixture,
        &Case {
            name: "declared_failed",
            producer: r#"bound retry "ready" > "$path""#,
            response: DECLARES_FAILED,
            ..Case::default()
        },
    );
    assert!(!declared_failed.attempted.recovery_succeeded);
    assert_eq!(
        declared_failed.attempted.failure_phase.as_deref(),
        Some("activity")
    );
    assert_eq!(declared_failed.attempted.decision, None, "not read");
    assert_eq!(declared_failed.deliveries, 1);
    assert!(declared_failed.failed_with_original());
}

#[test]
fn evidence_for_another_invocation_or_an_unsafe_path_admits_no_retry() {
    if !isolated("step_recovery::evidence_for_another_invocation_or_an_unsafe_path_admits_no_retry")
    {
        return;
    }
    let fixture = fixture();
    let cases: &[(&str, &str, &str)] = &[
        (
            "literal_backslash_n_suffix",
            r#"bound retry "x" > "$path"; printf '\\n' >> "$path""#,
            "literal backslash-n suffix",
        ),
        (
            "malformed",
            r#"printf '{"decision":' > "$path""#,
            "malformed",
        ),
        (
            "extra_field",
            r#"bound retry "x" | sed 's/}$/,"recovered":true}/' > "$path""#,
            "unknown field",
        ),
        (
            "unknown_verdict",
            r#"bound maybe "x" > "$path""#,
            "malformed",
        ),
        (
            "future_schema",
            r#"bound retry "x" | sed 's/"schema_version":1/"schema_version":2/' > "$path""#,
            "schema_version",
        ),
        (
            "stale_nonce",
            r#"nonce=0123456789abcdef0123456789abcdef; bound retry "replayed" > "$path""#,
            "nonce",
        ),
        (
            "other_run",
            r#"run=run-elsewhere; bound not_recovered "x" > "$path""#,
            "run_id",
        ),
        (
            "other_step",
            r#"step=push; bound retry "x" > "$path""#,
            "failed_step_id",
        ),
        (
            "other_attempt",
            r#"attempt=2; bound retry "x" > "$path""#,
            "attempt",
        ),
        (
            "symlink",
            r#"bound retry "x" > "$path.real"; ln -s "$path.real" "$path""#,
            "link or not a regular file",
        ),
        (
            "directory",
            r#"mkdir "$path""#,
            "link or not a regular file",
        ),
        (
            "oversized",
            r#"head -c 20000 /dev/zero | tr '\0' ' ' > "$path"; bound retry "x" >> "$path""#,
            "exceeds",
        ),
        (
            "blocker_without_kind",
            r#"bound external_blocker "x" | sed 's/}$/,"blocker":{"kind":"","evidence":"y"}}/' > "$path""#,
            "blocker needs a kind",
        ),
        (
            "external_blocker_without_blocker",
            r#"bound external_blocker "x" > "$path""#,
            "needs a blocker",
        ),
        (
            "retry_with_blocker",
            r#"bound retry "x" | sed 's/}$/,"blocker":{"kind":"k","evidence":"y"}}/' > "$path""#,
            "only an external_blocker",
        ),
        (
            "redirected_slot",
            r#"bound retry "x" > "$path.real"; d=$(dirname "$path"); mv "$d" "$d.moved"; ln -s "$d.moved" "$d""#,
            "replaced by a link",
        ),
    ];
    for (name, producer, detail) in cases {
        let observed = run(
            &fixture,
            &Case {
                name,
                producer,
                response: SAYS_RECOVERED,
                ..Case::default()
            },
        );
        assert_refused(&observed, name, "invalid", detail);
    }

    // The same invocation's earlier, valid file elsewhere in scratch is not
    // read: each invocation has its own path and nonce.
    let observed = run(
        &fixture,
        &Case {
            name: "elsewhere",
            producer: r#"bound not_recovered "x" > "$(dirname "$path")/decision.json""#,
            ..Case::default()
        },
    );
    assert_eq!(observed.decision().status, "absent");
    assert_eq!(
        observed.deliveries, 1,
        "ignored scratch is not a change to the worktree"
    );
}

#[test]
fn read_write_and_allocation_failures_are_never_a_verified_recovery() {
    if !isolated("step_recovery::read_write_and_allocation_failures_are_never_a_verified_recovery")
    {
        return;
    }
    let fixture = fixture();

    // The host refuses a slot directory that is a link out of the worktree,
    // before the provider launches.
    let unsafe_slot = run(
        &fixture,
        &Case {
            name: "unsafe_slot",
            prepare: |worktree| {
                let outside = worktree.with_file_name("outside");
                std::fs::create_dir_all(&outside).unwrap();
                std::fs::create_dir_all(worktree.join(".orbit")).unwrap();
                std::os::unix::fs::symlink(&outside, worktree.join(".orbit/tmp")).unwrap();
            },
            ..Case::default()
        },
    );
    assert!(!unsafe_slot.attempted.recovery_succeeded);
    assert_eq!(
        unsafe_slot.attempted.failure_phase.as_deref(),
        Some("decision")
    );
    assert!(
        unsafe_slot
            .attempted
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("is a link or not a directory")),
        "{:?}",
        unsafe_slot.attempted
    );
    assert!(
        unsafe_slot.envelopes.is_empty(),
        "the provider never launched"
    );
    assert_eq!(unsafe_slot.deliveries, 1);
    assert!(unsafe_slot.failed_with_original());

    // The leaf cannot create a child under the allocated file path. This is
    // a deterministic failed write even when tests run with elevated access.
    let unwritable = run(
        &fixture,
        &Case {
            name: "write_failure",
            producer: r#"bound not_recovered "x" > "$path/nope" 2>/dev/null"#,
            ..Case::default()
        },
    );
    assert_eq!(unwritable.decision().status, "absent");
    assert_eq!(unwritable.decision().verdict, None);
    assert!(!unwritable.decision().retry_admitted);
    assert_eq!(unwritable.deliveries, 1);

    // Inject a host read error after the producer has written a valid file.
    // This exercises fail-closed gate behavior without relying on permissions
    // that privileged test runners can bypass.
    let unreadable = run(
        &fixture,
        &Case {
            name: "read_failure",
            producer: r#"bound retry "x" > "$path""#,
            read_failure: true,
            ..Case::default()
        },
    );
    assert_refused(
        &unreadable,
        "read_failure",
        "unavailable",
        "injected decision read failure",
    );

    // Where file permissions are enforced for this process, also exercise the
    // composed reader's real I/O failure path. Root runners still execute the
    // injected host-read case above instead of silently returning early.
    let probe = fixture.root.join("probe");
    std::fs::write(&probe, "x").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&probe).is_err() {
        let unreadable_file = run(
            &fixture,
            &Case {
                name: "unreadable_file",
                producer: r#"bound retry "x" > "$path"; chmod 0000 "$path""#,
                ..Case::default()
            },
        );
        assert_refused(
            &unreadable_file,
            "unreadable_file",
            "unavailable",
            "Permission denied",
        );
    }
}

#[test]
fn a_claimed_follower_decides_from_its_own_run_local_file_without_owner_task_writes() {
    if !isolated(
        "step_recovery::a_claimed_follower_decides_from_its_own_run_local_file_without_owner_task_writes",
    ) {
        return;
    }
    let fixture = fixture();
    for (name, verdict, deliveries) in [
        ("claimed_declined", "not_recovered", 1),
        ("claimed_repaired", "retry", 2),
    ] {
        let producer = format!(r#"bound {verdict} "follower" > "$path""#);
        let observed = run(
            &fixture,
            &Case {
                name,
                producer: &producer,
                response: SAYS_RECOVERED,
                claimed: true,
                ..Case::default()
            },
        );
        assert_eq!(observed.decision().status, "verified", "{name}");
        assert_eq!(observed.decision().verdict.as_deref(), Some(verdict));
        assert_eq!(observed.deliveries, deliveries, "{name}");
        assert_eq!(observed.task_writes, 0, "{name}: no owner task write");
        let tools = observed.envelopes[0]["tools"].as_array().unwrap();
        assert!(
            !tools.iter().any(|tool| tool == "orbit.task.update"),
            "{name}: a claimed leaf is never granted orbit.task.update"
        );
        // [ORB-14260] The read stays granted; the run broker scopes it to the
        // claimed task.
        assert!(
            tools.iter().any(|tool| tool == "orbit.task.show"),
            "{name}: a claimed leaf may read its claimed task"
        );
    }
}

/// [ORB-14268] Step recovery declares a blocker outside the run: no
/// post-recovery attempt, final recovery skips it, and the failure activity
/// receives the typed blocker with its kind, as for an implementer blocker.
#[test]
fn an_external_blocker_decision_skips_the_retry_and_final_recovery() {
    if !isolated("step_recovery::an_external_blocker_decision_skips_the_retry_and_final_recovery") {
        return;
    }
    let fixture = fixture();
    let producer =
        format!(r#"bound external_blocker "operator must act" | {WITH_BLOCKER} > "$path""#);
    let blocked = run(
        &fixture,
        &Case {
            name: "external_blocker",
            producer: &producer,
            response: SAYS_RECOVERED,
            ..Case::default()
        },
    );
    let decision = blocked.decision();
    assert_eq!(decision.status, "verified");
    assert_eq!(decision.verdict.as_deref(), Some("external_blocker"));
    assert!(!decision.retry_admitted);
    assert!(
        decision
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("kind=missing_credentials")),
        "{decision:?}"
    );
    assert_eq!(
        blocked.envelopes.len(),
        1,
        "the recovery provider runs once"
    );
    assert_eq!(blocked.deliveries, 1, "no post-recovery attempt");
    assert!(blocked.post_recovery.is_empty());

    let message = match &blocked.result {
        Ok((false, Some(message))) => message.clone(),
        Err(message) => message.clone(),
        other => panic!("the step fails with the blocker: {other:?}"),
    };
    assert_eq!(
        orbit_types::workflow::task_blocked_by_agent_kind(&message),
        Some("missing_credentials"),
        "{message}"
    );
    assert!(message.contains(ORIGINAL_FAILURE), "{message}");

    assert!(
        matches!(
            blocked.final_recovery.as_slice(),
            [(outcome, Some(detail))] if outcome == "skipped" && detail.contains("declared a blocker")
        ),
        "final recovery skips the blocker: {:?}",
        blocked.final_recovery
    );
    let actions: Vec<&str> = blocked.hooks.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(
        actions,
        ["preserve_candidate"],
        "final recovery is never dispatched; the failure activity runs once"
    );
    let preserve = &blocked.hooks[0].1;
    assert_eq!(
        preserve["error_code"],
        orbit_types::workflow::TASK_BLOCKED_BY_AGENT_ERROR_CODE
    );
    assert_eq!(
        preserve["error_message"]
            .as_str()
            .and_then(orbit_types::workflow::task_blocked_by_agent_kind),
        Some("missing_credentials")
    );
}
