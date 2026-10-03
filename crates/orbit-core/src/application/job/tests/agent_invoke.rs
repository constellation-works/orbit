//! Who may admit an operator-only agent invocation, and why no other
//! submission path can manufacture one [ORB-11354].

use std::path::PathBuf;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::activity_job::TRUSTED_HOST_ADMISSION_KEY;
use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::OrbitRuntime;
use crate::application::job::{AGENT_INVOKE_JOB_ID, AgentInvokeRequest, seed_default_jobs};
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
fn a_runs_own_runner_grant_cannot_admit_an_invocation() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_runs_own_runner_grant_cannot_admit_an_invocation,
    )) {
        return;
    }
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

// -------------------------------------------------------------- forgery

#[test]
fn ordinary_job_input_cannot_carry_a_trusted_host_admission() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &ordinary_job_input_cannot_carry_a_trusted_host_admission,
    )) {
        return;
    }
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
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_foreground_job_run_cannot_carry_a_trusted_host_admission,
    )) {
        return;
    }
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

// ---------------------------------------------------- restart and retry

/// A run that carries an admission cannot be resumed: the admission covered one
/// invocation, and a resume would carry it into a run nobody authorized now.
#[test]
fn an_admitted_run_cannot_be_resumed() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &an_admitted_run_cannot_be_resumed,
    )) {
        return;
    }
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

/// [ORB-13560] Authorization precedes the keyed claim: a refused caller that
/// names a key leaves no run behind that a later operator retry would resolve.
#[test]
fn an_unauthorized_keyed_submission_claims_nothing() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &an_unauthorized_keyed_submission_claims_nothing,
    )) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime_with_codex_crew("workspace-write");
    let session = agent_session();
    let cwd = repo_root.display().to_string();
    let error = runtime
        .submit_agent_invoke_run(AgentInvokeRequest {
            idempotency_key: Some("incident-9"),
            ..request(&cwd, &session)
        })
        .expect_err("an agent must not start an unsandboxed process");
    assert_denied(error, "agent session with a retry key");
    assert_no_run_created(&runtime);
}

// ------------------------------------------------------ provider sandbox
