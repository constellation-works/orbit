//! Admission and lifecycle of the operator-only agent invocation [ORB-11354].
//!
//! The tests below are grouped by the question they answer:
//!
//! 1. Who may admit one (and who is refused before anything durable exists).
//! 2. What the admission records.
//! 3. Why no other submission path can manufacture the mode.
//! 4. How a finished invocation reads back, including the case where the
//!    provider exits 0 without finishing its turn.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::tool::{
    CallerIdentityProof, McpCapability, RemoteAgentInvokeMode, RemoteCallerGrant,
    ToolSessionContext,
};
use orbit_types::workflow::activity_job::TRUSTED_HOST_ADMISSION_KEY;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::OrbitRuntime;
use crate::application::job::pipeline::worker_command_override;
use crate::application::job::{
    AGENT_INVOKE_JOB_ID, AgentInvokeRequest, MAX_AGENT_INVOKE_TIMEOUT_SECONDS, agent_invoke_result,
    seed_default_jobs,
};
use crate::bootstrap::activity::seed_default_activities;

/// A runtime plus the checkout an invocation is admitted against.
fn test_runtime() -> (TempDir, OrbitRuntime, PathBuf) {
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

fn test_runtime_with_codex_crew(sandbox: &str) -> (TempDir, OrbitRuntime, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        format!(
            r#"[workflow]
default_crew = "system"

[crews.system]
provider = "codex"
model = "gpt-5.4"
backend = "cli"

[crews.opus]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"

[execution.codex]
sandbox = "{sandbox}"
"#
        ),
    )
    .expect("write crew config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    seed_default_jobs(&runtime.paths().global_dir.join("resources/jobs"), true).expect("seed jobs");
    seed_default_activities(
        &runtime.paths().global_dir.join("resources/activities"),
        true,
    )
    .expect("seed activities");
    (root, runtime, repo_root)
}

struct IdleWorker;

impl IdleWorker {
    fn install() -> Self {
        worker_command_override::set(["sh", "-c", "sleep 1"]);
        Self
    }
}

impl Drop for IdleWorker {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}

/// A session an MCP operator surface would present.
fn operator_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into_iter().collect(),
        ..ToolSessionContext::default()
    }
}

/// A session an ordinary agent MCP surface would present.
fn agent_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Agent].into_iter().collect(),
        ..ToolSessionContext::default()
    }
}

/// A run's own dispatcher stamp. `Runner` is deliberately absent from the
/// operation's allowed set, so this must be refused.
fn runner_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Runner].into_iter().collect(),
        ..ToolSessionContext::default()
    }
}

/// A remote caller the destination granted operator capability.
fn remote_operator_session(
    identity: CallerIdentityProof,
    agent_invoke: bool,
    agent_invoke_mode: Option<RemoteAgentInvokeMode>,
) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into_iter().collect(),
        remote_caller_grant: Some(RemoteCallerGrant {
            caller_machine_id: "hm_remote".to_string(),
            granted_capabilities: [McpCapability::Operator].into_iter().collect(),
            source: "mcp-callers.toml".to_string(),
            identity,
            agent_invoke,
            agent_invoke_mode,
        }),
        ..ToolSessionContext::default()
    }
}

fn request<'a>(cwd: &'a str, session: &'a ToolSessionContext) -> AgentInvokeRequest<'a> {
    AgentInvokeRequest {
        prompt: "why is the sweep clock restarting",
        cwd,
        crew: None,
        timeout_seconds: None,
        idempotency_key: None,
        provider_sandbox: None,
        actor: Some("human"),
        session_context: session,
    }
}

fn assert_denied(error: OrbitError, expectation: &str) {
    match error {
        OrbitError::CapabilityDenied(message) => assert!(
            message.contains("orbit.agent.invoke"),
            "{expectation}: the refusal must name the operation, got {message}"
        ),
        other => panic!("{expectation}: expected a capability denial, got {other:?}"),
    }
}

// ---------------------------------------------------------------- admission

#[test]
fn a_local_operator_session_keeps_the_existing_admission_path() {
    let (_root, runtime, _repo_root) = test_runtime();
    let authorizer = runtime
        .admit_agent_invoke(&operator_session())
        .expect("a local operator remains compatible");

    assert_eq!(authorizer.provenance.to_string(), "session");
    assert!(authorizer.remote_caller.is_none());
}

#[test]
fn an_agent_session_cannot_admit_an_invocation() {
    let (_root, runtime, repo_root) = test_runtime();
    let session = agent_session();
    let error = runtime
        .submit_agent_invoke_run(request(&repo_root.display().to_string(), &session))
        .expect_err("an agent must not start an unsandboxed process");
    assert_denied(error, "agent session");
    assert_no_run_created(&runtime);
}

#[test]
fn a_runs_own_runner_grant_cannot_admit_an_invocation() {
    // The mode exists to leave the sandbox a managed run executes inside, so a
    // run that could admit itself would be a sandbox escape wearing an
    // authorization. Every other run-reachable governed operation lists
    // `runner`; this one deliberately does not.
    let (_root, runtime, repo_root) = test_runtime();
    let session = runner_session();
    let error = runtime
        .submit_agent_invoke_run(request(&repo_root.display().to_string(), &session))
        .expect_err("a managed run must not admit an unsandboxed process");
    assert_denied(error, "runner session");
    assert_no_run_created(&runtime);
}

#[test]
fn a_self_asserted_remote_operator_cannot_admit_an_invocation() {
    let (_root, runtime, repo_root) = test_runtime();
    let session = remote_operator_session(CallerIdentityProof::SelfAsserted, true, None);
    let error = runtime
        .submit_agent_invoke_run(request(&repo_root.display().to_string(), &session))
        .expect_err("a spoofable remote identity must not admit an unsandboxed process");
    match error {
        OrbitError::CapabilityDenied(message) => {
            assert!(
                message.contains("identity is self-asserted"),
                "the refusal must explain why the remote identity is insufficient: {message}"
            );
        }
        other => panic!("expected a capability denial, got {other:?}"),
    }
    assert_no_run_created(&runtime);
}

#[test]
fn a_key_bound_remote_operator_still_needs_the_explicit_operation_grant() {
    let (_root, runtime, _repo_root) = test_runtime();
    let session = remote_operator_session(CallerIdentityProof::KeyBound, false, None);
    let error = runtime
        .admit_agent_invoke(&session)
        .expect_err("operator capability alone must not grant unsandboxed remote execution");

    match error {
        OrbitError::CapabilityDenied(message) => assert!(
            message.contains("does not enable `agent_invoke`"),
            "the refusal must name the missing destination grant: {message}"
        ),
        other => panic!("expected a capability denial, got {other:?}"),
    }
}

#[test]
fn an_explicit_key_bound_remote_grant_preserves_the_real_caller() {
    let (_root, runtime, _repo_root) = test_runtime();
    let session = remote_operator_session(CallerIdentityProof::KeyBound, true, None);

    let authorizer = runtime
        .admit_agent_invoke(&session)
        .expect("the explicit authenticated remote grant admits one invocation");
    let remote = authorizer
        .remote_caller
        .expect("remote admission retains destination-resolved caller facts");

    assert_eq!(authorizer.provenance.to_string(), "remote-grant");
    assert_eq!(remote.caller_machine_id, "hm_remote");
    assert_eq!(remote.identity, CallerIdentityProof::KeyBound);
    assert!(remote.agent_invoke);
}

#[test]
fn an_explicit_cooperative_remote_operator_retains_self_asserted_provenance() {
    let (_root, runtime, _repo_root) = test_runtime();
    let session = remote_operator_session(
        CallerIdentityProof::SelfAsserted,
        true,
        Some(RemoteAgentInvokeMode::Cooperative),
    );

    let authorizer = runtime
        .admit_agent_invoke(&session)
        .expect("the destination explicitly trusted its cooperative SSH operator channel");
    let remote = authorizer.remote_caller.expect("remote caller facts");

    assert_eq!(remote.identity, CallerIdentityProof::SelfAsserted);
    assert_eq!(
        remote.agent_invoke_mode,
        Some(RemoteAgentInvokeMode::Cooperative)
    );
}

#[test]
fn revoking_the_remote_operation_grant_denies_the_next_invocation() {
    let (_root, runtime, _repo_root) = test_runtime();
    let mut session = remote_operator_session(CallerIdentityProof::KeyBound, true, None);
    runtime
        .admit_agent_invoke(&session)
        .expect("the initial explicit grant is admitted");

    session
        .remote_caller_grant
        .as_mut()
        .expect("remote grant")
        .agent_invoke = false;
    let error = runtime
        .admit_agent_invoke(&session)
        .expect_err("revocation must deny the next admission");

    assert_denied(error, "revoked remote operation grant");
}

#[test]
fn an_unidentified_caller_cannot_admit_an_invocation() {
    let (_root, runtime, repo_root) = test_runtime();
    let session = ToolSessionContext::default();
    // The process running these tests is neither an interactive terminal nor
    // an operator override, so the envelope resolves to nothing at all.
    let error = runtime
        .submit_agent_invoke_run(request(&repo_root.display().to_string(), &session))
        .expect_err("ambiguity must fail closed");
    assert_denied(error, "unidentified caller");
    assert_no_run_created(&runtime);
}

fn assert_no_run_created(runtime: &OrbitRuntime) {
    let runs = runtime
        .list_job_runs(crate::application::job::JobRunListParams::default())
        .expect("list runs");
    assert!(
        runs.is_empty(),
        "a refused caller must not leave a durable run behind"
    );
}

// --------------------------------------------------------------- validation
//
// Validation runs *after* admission, so these use an operator session. They
// assert the invocation's own contract: an explicit, containable cwd and a
// bounded timeout.

#[test]
fn a_cwd_outside_the_workspace_checkout_is_refused() {
    let (root, runtime, _repo_root) = test_runtime();
    let outside = root.path().join("elsewhere");
    std::fs::create_dir_all(&outside).expect("create outside dir");
    let session = operator_session();
    let error = runtime
        .submit_agent_invoke_run(request(&outside.display().to_string(), &session))
        .expect_err("an admission is scoped to the workspace that granted it");
    match error {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("outside workspace"), "{message}");
            assert!(message.contains("checkout"), "{message}");
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[test]
fn a_relative_or_missing_cwd_is_refused() {
    let (_root, runtime, repo_root) = test_runtime();
    let session = operator_session();
    for (cwd, expected) in [("crates", "must be an absolute path"), ("", "is required")] {
        match runtime
            .submit_agent_invoke_run(request(cwd, &session))
            .expect_err("cwd must be explicit and absolute")
        {
            OrbitError::InvalidInput(message) => assert!(message.contains(expected), "{message}"),
            other => panic!("expected invalid input, got {other:?}"),
        }
    }
    let absent = repo_root.join("does-not-exist");
    match runtime
        .submit_agent_invoke_run(request(&absent.display().to_string(), &session))
        .expect_err("cwd must exist")
    {
        OrbitError::InvalidInput(message) => assert!(message.contains("not readable"), "{message}"),
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[test]
fn an_empty_prompt_is_refused() {
    let (_root, runtime, repo_root) = test_runtime();
    let session = operator_session();
    let cwd = repo_root.display().to_string();
    let mut request = request(&cwd, &session);
    request.prompt = "   ";
    match runtime
        .submit_agent_invoke_run(request)
        .expect_err("an invocation with nothing to do still starts a process")
    {
        OrbitError::InvalidInput(message) => assert!(message.contains("`prompt`"), "{message}"),
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[test]
fn a_timeout_beyond_the_ceiling_is_refused_rather_than_clamped() {
    let (_root, runtime, repo_root) = test_runtime();
    let session = operator_session();
    let cwd = repo_root.display().to_string();
    let mut request = request(&cwd, &session);
    request.timeout_seconds = Some(MAX_AGENT_INVOKE_TIMEOUT_SECONDS + 1);
    match runtime
        .submit_agent_invoke_run(request)
        .expect_err("an operator who asked for too long should learn that they did")
    {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("at most"), "{message}")
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
}

// -------------------------------------------------------------- forgery

#[test]
fn ordinary_job_input_cannot_carry_a_trusted_host_admission() {
    // The reserved key is refused on the single path every submission surface
    // funnels through, so this covers `orbit run job`, a tool call, and an
    // automation key alike.
    let (_root, runtime, _repo_root) = test_runtime();
    let forged = json!({
        "prompt": "why",
        TRUSTED_HOST_ADMISSION_KEY: {
            "authorized_by": "not-an-operator",
            "authorizer_provenance": "session",
            "authorized_at": Utc::now().to_rfc3339(),
            "workspace_path": "/",
            "cwd": "/",
        },
    });
    let error = runtime
        .submit_pipeline_run(AGENT_INVOKE_JOB_ID, forged, None, Some("agent"))
        .expect_err("run input must not be able to manufacture the mode");
    match error {
        OrbitError::InvalidInput(message) => {
            assert_eq!(
                message,
                "run input for job 'agent_invoke_pipeline' set the reserved \
                 `trusted_host_admission` field; trusted host execution is admitted per \
                 invocation by the governed `orbit.agent.invoke` operation and cannot \
                 be requested through ordinary job input"
            );
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
}

#[test]
fn a_foreground_job_run_cannot_carry_a_trusted_host_admission() {
    let (root, runtime, _repo_root) = test_runtime();
    let job_path = root.path().join("forged.yaml");
    std::fs::write(
        &job_path,
        r#"schemaVersion: 2
kind: Job
metadata:
  name: forged_pipeline
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
  steps:
    - id: noop
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("write job");

    let error = runtime
        .run_job_v2_from_yaml(
            &job_path,
            json!({ TRUSTED_HOST_ADMISSION_KEY: { "authorized_by": "x" } }),
        )
        .expect_err("the foreground path takes caller input too");
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");
}

// ------------------------------------------------------------ result reading

fn finished_run(state: JobRunState, output: Value) -> (JobRun, BTreeMap<u32, Value>) {
    let run = JobRun {
        run_id: "jrun-test-1".to_string(),
        job_id: AGENT_INVOKE_JOB_ID.to_string(),
        attempt: 1,
        state,
        scheduled_at: Utc::now(),
        started_at: Some(Utc::now()),
        finished_at: Some(Utc::now()),
        duration_ms: Some(10),
        created_at: Utc::now(),
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    (run, BTreeMap::from([(0, output)]))
}

#[test]
fn a_provider_that_exits_zero_without_finishing_is_not_a_successful_investigation() {
    // The whole point of the projection: the exit code is present and zero, and
    // the answer to "did this succeed" is still no, because the run recorded a
    // failure when the envelope never terminated.
    let (run, outputs) = finished_run(
        JobRunState::Failed,
        json!({
            "exit_code": 0,
            "timed_out": false,
            "completion_envelope_satisfied": false,
            "completion_envelope_error": "no terminating envelope",
            "stdout_text": "thinking...",
            "stdout_blob_ref": "blob-7",
        }),
    );
    let result = agent_invoke_result(&run, Some(&outputs)).expect("an agent invocation run");
    assert_eq!(result.outcome, "failed");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.completed_envelope);
    assert_eq!(
        result.failure_reason.as_deref(),
        Some("no terminating envelope")
    );
}

#[test]
fn a_timeout_and_a_cancellation_read_differently() {
    let (timed_out, outputs) = finished_run(
        JobRunState::Timeout,
        json!({ "timed_out": true, "exit_code": Value::Null }),
    );
    let result = agent_invoke_result(&timed_out, Some(&outputs)).expect("result");
    assert_eq!(result.outcome, "timeout");
    assert!(result.timed_out);

    let (cancelled, outputs) = finished_run(JobRunState::Cancelled, json!({}));
    let result = agent_invoke_result(&cancelled, Some(&outputs)).expect("result");
    assert_eq!(result.outcome, "cancelled");
    assert!(!result.timed_out);
    // Nothing reported, so nothing is claimed about the envelope.
    assert!(!result.completed_envelope);
}

#[test]
fn the_preview_is_bounded_and_names_where_the_rest_lives() {
    let long = "x".repeat(10_000);
    let (run, outputs) = finished_run(
        JobRunState::Success,
        json!({
            "exit_code": 0,
            "completion_envelope_satisfied": true,
            "summary": "the clock restarts on config reload",
            "stdout_text": long,
            "stdout_blob_ref": "blob-9",
        }),
    );
    let result = agent_invoke_result(&run, Some(&outputs)).expect("result");
    assert_eq!(result.outcome, JobRunState::Success.to_string());
    assert!(result.completed_envelope);
    assert_eq!(
        result.summary.as_deref(),
        Some("the clock restarts on config reload")
    );
    let preview = result.preview.expect("a preview");
    assert!(preview.len() < 10_000, "preview must be bounded");
    assert!(result.preview_truncated);
    assert_eq!(result.stdout_blob_ref.as_deref(), Some("blob-9"));
}

#[test]
fn an_unrelated_run_has_no_agent_invocation_projection() {
    let (mut run, outputs) = finished_run(JobRunState::Success, json!({}));
    run.job_id = "task_auto_pipeline".to_string();
    assert!(agent_invoke_result(&run, Some(&outputs)).is_none());
}

#[test]
fn run_show_projects_the_persisted_provider_sandbox() {
    let (mut run, outputs) = finished_run(JobRunState::Success, json!({}));
    run.input = Some(json!({
        "provider_sandbox": "codex:danger-full-access",
    }));
    let result = agent_invoke_result(&run, Some(&outputs)).expect("result");
    assert_eq!(
        result.provider_sandbox.as_deref(),
        Some("codex:danger-full-access")
    );
}

// ---------------------------------------------------- restart and retry

/// A run that carries an admission cannot be resumed: the admission covered one
/// invocation, and a resume would carry it into a run nobody authorized now.
#[test]
fn an_admitted_run_cannot_be_resumed() {
    let (_root, runtime, _repo_root) = test_runtime();
    let admitted_input = json!({
        "prompt": "why",
        TRUSTED_HOST_ADMISSION_KEY: {
            "authorized_by": "human",
            "authorizer_provenance": "interactive-terminal",
            "authorized_at": Utc::now().to_rfc3339(),
            "workspace_path": "/checkout",
            "cwd": "/checkout",
        },
    });
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            AGENT_INVOKE_JOB_ID,
            1,
            Utc::now(),
            Some(admitted_input.clone()),
            None,
        )
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start run");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(10))
        .expect("finalize run as failed");

    let error = runtime
        .submit_resume_run(&run.run_id, Some("human"), None)
        .expect_err("an admission covers one invocation only");
    match error {
        OrbitError::JobValidation(message) => {
            assert_eq!(
                message,
                format!(
                    "job run '{}' was an operator-admitted trusted host invocation \
                     and cannot be resumed; its admission covered that invocation only. \
                     Submit a new `orbit.agent.invoke` to authorize another one",
                    run.run_id
                )
            );
        }
        other => panic!("expected a job validation refusal, got {other:?}"),
    }
}

/// A resubmission carrying a key a previous submission used resolves that run
/// rather than starting a second unsandboxed agent.
///
/// Terminal state is deliberately not a factor: retrying after the original
/// finished should hand back the original result, not re-run the investigation.
#[test]
fn a_repeated_idempotency_key_resolves_the_original_run() {
    let (_root, runtime, _repo_root) = test_runtime();
    let first = runtime
        .stores()
        .jobs()
        .insert_job_run(
            AGENT_INVOKE_JOB_ID,
            1,
            Utc::now(),
            Some(json!({ "prompt": "why", "idempotency_key": "incident-4821" })),
            None,
        )
        .expect("insert first run");

    let (resolved, deduplicated) = runtime
        .submit_trusted_host_pipeline_run(
            json!({ "prompt": "why", "cwd": "/checkout" }),
            "human",
            Some("incident-4821"),
        )
        .expect("a retry resolves rather than failing");

    assert!(deduplicated, "a repeated key must not start a second agent");
    assert_eq!(resolved.run_id, first.run_id);
    assert_eq!(resolved.job_name, AGENT_INVOKE_JOB_ID);

    let runs = runtime
        .list_job_runs(crate::application::job::JobRunListParams::default())
        .expect("list runs");
    assert_eq!(runs.len(), 1, "no second run may be created");
}

/// A blank key is no key: it must not collide with every other blank one.
#[test]
fn a_blank_idempotency_key_is_treated_as_absent() {
    let (_root, runtime, _repo_root) = test_runtime();
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            AGENT_INVOKE_JOB_ID,
            1,
            Utc::now(),
            Some(json!({ "prompt": "why", "idempotency_key": "" })),
            None,
        )
        .expect("insert run");

    // Submission proceeds past the dedupe check and fails later for a reason
    // that is not "resolved an existing run" — the catalog has no seeded job in
    // this bare fixture. What matters is that a blank key did not match.
    let error = runtime
        .submit_trusted_host_pipeline_run(
            json!({ "prompt": "why", "cwd": "/checkout" }),
            "human",
            Some("   "),
        )
        .expect_err("the bare fixture has no seeded job catalog");
    assert!(
        matches!(error, OrbitError::NotFound { .. }),
        "expected the submission to reach catalog resolution, got {error:?}"
    );
}

// ------------------------------------------------------ provider sandbox

#[test]
fn a_danger_full_access_codex_invocation_warns_and_records_provider_sandbox() {
    let _worker = IdleWorker::install();
    let (_root, runtime, repo_root) = test_runtime_with_codex_crew("danger-full-access");
    let session = operator_session();
    let cwd = repo_root.display().to_string();
    let submission = runtime
        .submit_agent_invoke_run(request(&cwd, &session))
        .expect("submit");

    assert_eq!(submission.provider_sandbox, "codex:danger-full-access");
    assert_eq!(
        submission.warnings,
        vec![
            "provider runs with codex:danger-full-access; it may use host integrations \
             (browser, computer use, …) beyond the working directory"
                .to_string()
        ]
    );

    let run = runtime
        .stores()
        .jobs()
        .get_job_run(&submission.run_id)
        .expect("load run")
        .expect("run exists");
    assert_eq!(
        run.input
            .as_ref()
            .and_then(|input| input.get("provider_sandbox"))
            .and_then(Value::as_str),
        Some("codex:danger-full-access")
    );
    let result = agent_invoke_result(&run, None).expect("projection");
    assert_eq!(
        result.provider_sandbox.as_deref(),
        Some("codex:danger-full-access")
    );
}

#[test]
fn a_codex_read_only_override_is_honoured_and_does_not_warn() {
    let _worker = IdleWorker::install();
    let (_root, runtime, repo_root) = test_runtime_with_codex_crew("danger-full-access");
    let session = operator_session();
    let cwd = repo_root.display().to_string();
    let mut request = request(&cwd, &session);
    request.provider_sandbox = Some("read-only");
    let submission = runtime
        .submit_agent_invoke_run(request)
        .expect("submit with override");

    assert_eq!(submission.provider_sandbox, "codex:read-only");
    assert!(submission.warnings.is_empty());
    let run = runtime
        .stores()
        .jobs()
        .get_job_run(&submission.run_id)
        .expect("load run")
        .expect("run exists");
    assert_eq!(
        run.input
            .as_ref()
            .and_then(|input| input.get("provider_sandbox"))
            .and_then(Value::as_str),
        Some("codex:read-only")
    );
}

#[test]
fn an_unsupported_provider_sandbox_override_is_refused() {
    let (_root, runtime, repo_root) = test_runtime_with_codex_crew("workspace-write");
    let session = operator_session();
    let cwd = repo_root.display().to_string();
    let mut request = request(&cwd, &session);
    request.provider_sandbox = Some("unrestricted");
    match runtime
        .submit_agent_invoke_run(request)
        .expect_err("unsupported mode")
    {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("`provider_sandbox`"), "{message}");
            assert!(message.contains("unrestricted"), "{message}");
            assert!(message.contains("read-only"), "{message}");
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
    assert_no_run_created(&runtime);
}

#[test]
fn a_codex_sandbox_mode_is_refused_for_claude() {
    let (_root, runtime, repo_root) = test_runtime_with_codex_crew("workspace-write");
    let session = operator_session();
    let cwd = repo_root.display().to_string();
    let mut request = request(&cwd, &session);
    request.crew = Some("opus");
    request.provider_sandbox = Some("read-only");
    match runtime
        .submit_agent_invoke_run(request)
        .expect_err("claude has no inner-sandbox override")
    {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("claude"), "{message}");
            assert!(message.contains("default"), "{message}");
        }
        other => panic!("expected invalid input, got {other:?}"),
    }
    assert_no_run_created(&runtime);
}
