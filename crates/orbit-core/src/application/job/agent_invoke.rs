//! Operator-only host exploration/debugging invocation [ORB-11354].
//!
//! One durable, asynchronous run of one provider subprocess, outside the
//! executor's filesystem sandbox, in an explicit working directory. It exists
//! for the question an operator cannot answer from inside a managed run's
//! sandbox — "why is this host behaving this way" — and it deliberately reuses
//! the ordinary job machinery rather than adding a second scheduler: the same
//! run record, the same detached worker, the same supervision, the same
//! `orbit run show|logs|cancel`.
//!
//! # What this module owns
//!
//! Exactly the admission and the shape of the submission:
//!
//! * the operator check (delegated to
//!   [`OrbitRuntime::admit_agent_invoke`](crate::OrbitRuntime), the single
//!   canonical chokepoint),
//! * validation of the explicit workspace/cwd pair, prompt, crew and timeout,
//! * stamping the durable [`TrustedHostAdmission`] the engine requires,
//! * the bounded operator-facing projection of a finished invocation.
//!
//! Everything after submission is the existing pipeline: nothing here spawns,
//! supervises, cancels, or mutates a task.
//!
//! # What it is not
//!
//! Not a task, and not a delivery pipeline. The run performs no task
//! transition, opens no PR, and dispatches nothing. The child subprocess
//! carries managed-run provenance, so it resolves as an *agent* at every
//! capability chokepoint and cannot admit another invocation of its own.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::Provider;
use orbit_types::workflow::activity_job::{
    DEFAULT_PROVIDER_SANDBOX, TRUSTED_HOST_ADMISSION_KEY, TrustedHostAdmission,
    admit_provider_sandbox_mode, format_provider_sandbox, is_least_restrictive_provider_sandbox,
    least_restrictive_provider_sandbox_warning,
};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::pipeline::{PipelineInvokeResult, PipelineSubmission, input_hash};

/// Catalog job that carries one agent invocation.
pub const AGENT_INVOKE_JOB_ID: &str = "agent_invoke_pipeline";

/// Wall-clock bound applied when the caller does not name one. The activity
/// asset's own `wall_clock_timeout_seconds` remains the ceiling; a request may
/// only shorten it.
pub const DEFAULT_AGENT_INVOKE_TIMEOUT_SECONDS: u64 = 1800;

/// Longest bound a caller may request. Anything above this is refused rather
/// than silently clamped, so an operator who asked for a day-long unsandboxed
/// process learns that they did.
pub const MAX_AGENT_INVOKE_TIMEOUT_SECONDS: u64 = 7200;

/// Run-input field carrying a caller's agent-invocation retry key [ORB-11354].
const AGENT_INVOKE_IDEMPOTENCY_KEY_FIELD: &str = "idempotency_key";

/// Cap on the run history scanned when matching an agent-invocation retry key.
const AGENT_INVOKE_IDEMPOTENCY_SCAN_LIMIT: usize = 200;

/// How much of the provider's captured output the operator-facing projection
/// inlines. The full capture stays addressable through the durable blob
/// reference the projection carries alongside it.
const RESULT_PREVIEW_LIMIT_BYTES: usize = 4096;

/// One operator's request to run an exploration invocation.
#[derive(Debug, Clone)]
pub struct AgentInvokeRequest<'a> {
    /// What the operator wants investigated. Required and non-empty: an
    /// invocation with nothing to do would still start an unsandboxed process.
    pub prompt: &'a str,
    /// Working directory the provider subprocess starts in. Required and
    /// explicit — never inferred from the caller's cwd, because the caller may
    /// be an MCP server in an unrelated directory.
    pub cwd: &'a str,
    /// Crew selecting provider/model/effort. `None` uses the workspace default.
    pub crew: Option<&'a str>,
    /// Requested wall-clock bound, in seconds.
    pub timeout_seconds: Option<u64>,
    /// Caller-supplied retry key. Two submissions carrying the same key resolve
    /// to the same run rather than starting a second subprocess.
    pub idempotency_key: Option<&'a str>,
    /// Per-invocation inner-sandbox override (`read-only`, `workspace-write`,
    /// `danger-full-access` for Codex). `None` uses the crew provider's
    /// configured default. Values the provider does not support are refused.
    pub provider_sandbox: Option<&'a str>,
    /// Attribution label for the authorizing operator.
    pub actor: Option<&'a str>,
    /// The caller's session, resolved at the admission chokepoint.
    pub session_context: &'a ToolSessionContext,
}

/// The durable outcome of a successful submission.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInvokeSubmission {
    pub run_id: String,
    pub job_id: String,
    pub submitted_at: String,
    /// Whether the run is waiting on the job's `max_active_runs` ceiling rather
    /// than already executing.
    pub queued: bool,
    /// Whether this submission resolved an existing run through its
    /// idempotency key instead of creating a new one.
    pub deduplicated: bool,
    /// The admission recorded on the run.
    pub admission: TrustedHostAdmission,
    /// Effective wall-clock bound for the invocation.
    pub timeout_seconds: u64,
    /// Effective provider inner sandbox, as `provider:mode` (for example
    /// `codex:danger-full-access`). Distinct from Orbit's executor sandbox.
    pub provider_sandbox: String,
    /// Operator-facing warnings. Non-empty when `provider_sandbox` is the
    /// provider's least-restrictive inner sandbox.
    pub warnings: Vec<String>,
}

/// A finished (or running) invocation, rendered for an operator.
///
/// Deliberately distinguishes the ways an invocation can end. A provider that
/// exits 0 without terminating its response envelope did not finish its turn,
/// and the run records that as a failure — so `outcome` here is never derived
/// from the exit code alone.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentInvokeResult {
    /// `running`, `queued`, `succeeded`, `failed`, `timeout`, `cancelled`,
    /// or `interrupted` — the run's own state, not the subprocess's exit code.
    pub outcome: String,
    /// Why a non-success outcome happened, when the run recorded a reason.
    pub failure_reason: Option<String>,
    /// The provider's exit code, when the subprocess ran to completion.
    pub exit_code: Option<i64>,
    /// Whether the subprocess was killed for exceeding its wall-clock bound.
    pub timed_out: bool,
    /// Whether the invocation terminated with a well-formed response envelope.
    /// `false` on an otherwise-clean exit means the agent stopped mid-turn.
    pub completed_envelope: bool,
    /// The agent's own summary, when it returned one.
    pub summary: Option<String>,
    /// Bounded preview of the captured output.
    pub preview: Option<String>,
    /// Whether `preview` is shorter than what the invocation actually produced.
    pub preview_truncated: bool,
    /// Durable reference to the complete captured stdout, readable with
    /// `orbit run logs <RUN_ID>` after the preview is exhausted.
    pub stdout_blob_ref: Option<String>,
    /// Effective provider inner sandbox persisted at submission, as
    /// `provider:mode`. Absent on runs submitted before this field existed.
    pub provider_sandbox: Option<String>,
}

impl OrbitRuntime {
    /// Admit and submit one exploration invocation.
    ///
    /// Order is the contract: the operator check happens before validation,
    /// and both happen before anything durable exists. An unauthorized caller
    /// therefore never creates a run record, a worktree, or a process.
    pub fn submit_agent_invoke_run(
        &self,
        request: AgentInvokeRequest<'_>,
    ) -> Result<AgentInvokeSubmission, OrbitError> {
        let authorizer = self.admit_agent_invoke(request.session_context)?;

        let prompt = require_non_empty(request.prompt, "prompt")?;
        let cwd = self.resolve_workspace_cwd("cwd", request.cwd)?;
        let crew = self.canonical_crew_name(request.crew)?;
        let timeout_seconds = resolve_timeout(request.timeout_seconds)?;
        let provider_sandbox =
            self.resolve_invocation_provider_sandbox(request.crew, request.provider_sandbox)?;
        let requested_actor = request
            .actor
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| self.actor_label().to_string(), ToOwned::to_owned);
        let actor = authorizer
            .remote_caller
            .as_ref()
            .map(|grant| grant.caller_machine_id.clone())
            .unwrap_or(requested_actor);

        let admission = TrustedHostAdmission {
            authorized_by: actor.clone(),
            authorizer_provenance: authorizer.provenance.to_string(),
            caller_machine_id: authorizer
                .remote_caller
                .as_ref()
                .map(|grant| grant.caller_machine_id.clone()),
            caller_identity: authorizer
                .remote_caller
                .as_ref()
                .map(|grant| grant.identity),
            agent_invoke_mode: authorizer
                .remote_caller
                .as_ref()
                .and_then(|grant| grant.agent_invoke_mode),
            authorized_at: Utc::now().to_rfc3339(),
            workspace_path: self.paths().repo_root.display().to_string(),
            cwd: cwd.display().to_string(),
        };

        let input = json!({
            "prompt": prompt,
            "cwd": cwd.display().to_string(),
            "crew": crew,
            "timeout_seconds": timeout_seconds,
            "provider_sandbox": provider_sandbox.label,
            TRUSTED_HOST_ADMISSION_KEY: serde_json::to_value(&admission)
                .map_err(|error| OrbitError::Execution(format!("encode admission: {error}")))?,
        });

        let (invoke, deduplicated) =
            self.submit_trusted_host_pipeline_run(input, &actor, request.idempotency_key)?;

        tracing::warn!(
            target: "orbit.trusted_host",
            run_id = %invoke.run_id,
            authorized_by = %admission.authorized_by,
            authorizer_provenance = %admission.authorizer_provenance,
            cwd = %admission.cwd,
            provider_sandbox = %provider_sandbox.label,
            deduplicated,
            "admitted an operator agent invocation outside the executor sandbox"
        );
        if let Some(warning) = provider_sandbox.warning.as_deref() {
            tracing::warn!(
                target: "orbit.trusted_host",
                run_id = %invoke.run_id,
                provider_sandbox = %provider_sandbox.label,
                "{warning}"
            );
        }

        Ok(AgentInvokeSubmission {
            run_id: invoke.run_id,
            job_id: invoke.job_name,
            submitted_at: invoke.submitted_at,
            queued: invoke.queued,
            deduplicated,
            admission,
            timeout_seconds,
            provider_sandbox: provider_sandbox.label,
            warnings: provider_sandbox.warning.into_iter().collect(),
        })
    }

    /// Resolve the provider inner sandbox for this invocation.
    ///
    /// The crew selects the provider; Codex then reads
    /// `[execution.codex].sandbox` unless the caller overrode the mode.
    /// Other providers have no configurable inner sandbox, so the label is
    /// `{provider}:default` and any other override is refused.
    fn resolve_invocation_provider_sandbox(
        &self,
        crew: Option<&str>,
        override_mode: Option<&str>,
    ) -> Result<ResolvedProviderSandbox, OrbitError> {
        let resolved_crew = self.resolve_crew_for_task(crew, None)?;
        let provider = Provider::parse(&resolved_crew.assignment.provider).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "crew '{}' names an unknown provider: {error}",
                resolved_crew.name
            ))
        })?;
        let default_mode = match provider {
            Provider::Codex => self.codex_execution_policy().sandbox().to_string(),
            _ => DEFAULT_PROVIDER_SANDBOX.to_string(),
        };
        let mode = match override_mode
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(requested) => admit_provider_sandbox_mode(provider, requested)
                .map(str::to_string)
                .map_err(OrbitError::InvalidInput)?,
            None => default_mode,
        };
        let label = format_provider_sandbox(provider, &mode);
        let warning = is_least_restrictive_provider_sandbox(provider, &mode)
            .then(|| least_restrictive_provider_sandbox_warning(&label));
        Ok(ResolvedProviderSandbox { label, warning })
    }
}

struct ResolvedProviderSandbox {
    label: String,
    warning: Option<String>,
}

/// Read a finished or in-flight invocation for an operator [ORB-11354].
///
/// Returns `None` for any run that is not an agent invocation, so the ordinary
/// run projections can call it unconditionally.
///
/// Reads the run record and its persisted state rather than the live process:
/// results survive the submitting client disconnecting, and stay readable long
/// after the subprocess is gone.
pub fn agent_invoke_result(
    run: &JobRun,
    step_outputs: Option<&std::collections::BTreeMap<u32, Value>>,
) -> Option<AgentInvokeResult> {
    if run.job_id != AGENT_INVOKE_JOB_ID {
        return None;
    }
    let output = step_outputs.and_then(|outputs| outputs.values().next_back());
    let field = |key: &str| output.and_then(|value| value.get(key));

    let timed_out = field("timed_out").and_then(Value::as_bool).unwrap_or(false);
    let preview = field("stdout_text")
        .and_then(Value::as_str)
        .map(bounded_preview);
    let preview_truncated = field("stdout_text_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || field("stdout_text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.len() > RESULT_PREVIEW_LIMIT_BYTES);

    Some(AgentInvokeResult {
        outcome: outcome_label(run),
        failure_reason: run
            .steps
            .last()
            .and_then(|step| step.error_message.clone())
            .or_else(|| {
                field("completion_envelope_error")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }),
        exit_code: field("exit_code").and_then(Value::as_i64),
        timed_out,
        // Absent output means the invocation never reported, which is not the
        // same as reporting a complete envelope. Default to `false`.
        completed_envelope: field("completion_envelope_satisfied")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary: field("summary")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        preview,
        preview_truncated,
        stdout_blob_ref: field("stdout_blob_ref")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        provider_sandbox: run
            .input
            .as_ref()
            .and_then(|input| input.get("provider_sandbox"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// The run state, rendered as the outcome vocabulary an operator reads.
///
/// Taken from the run record, never from the provider's exit code: a
/// subprocess that exits 0 without finishing its turn produces a `failed` run,
/// and that is the answer to "did the investigation succeed".
fn outcome_label(run: &JobRun) -> String {
    run.state.to_string()
}

fn bounded_preview(text: &str) -> String {
    if text.len() <= RESULT_PREVIEW_LIMIT_BYTES {
        return text.to_string();
    }
    let mut end = RESULT_PREVIEW_LIMIT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn require_non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str, OrbitError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(format!("`{field}` is required")));
    }
    Ok(trimmed)
}

fn resolve_timeout(requested: Option<u64>) -> Result<u64, OrbitError> {
    let Some(seconds) = requested else {
        return Ok(DEFAULT_AGENT_INVOKE_TIMEOUT_SECONDS);
    };
    if seconds == 0 {
        return Err(OrbitError::InvalidInput(
            "`timeout_seconds` must be at least 1".to_string(),
        ));
    }
    if seconds > MAX_AGENT_INVOKE_TIMEOUT_SECONDS {
        return Err(OrbitError::InvalidInput(format!(
            "`timeout_seconds` must be at most {MAX_AGENT_INVOKE_TIMEOUT_SECONDS}"
        )));
    }
    Ok(seconds)
}

impl OrbitRuntime {
    /// Persist and dispatch one operator-admitted trusted-host invocation
    /// [ORB-11354].
    ///
    /// The only submission permitted to write [`TRUSTED_HOST_ADMISSION_KEY`],
    /// which is why it is here — on the module that owns the refusal — rather
    /// than assembling a `PipelineSubmission` from outside.
    ///
    /// `idempotency_key` makes a retried submission resolve the run the first
    /// attempt created instead of starting a second subprocess. Keys are
    /// matched over a bounded window of this job's recent runs, the same shape
    /// the ship guard uses: a key older than that window is not recognized and
    /// submits again, which is why a key is a retry handle rather than a
    /// permanent uniqueness constraint.
    pub(super) fn submit_trusted_host_pipeline_run(
        &self,
        input: Value,
        actor: &str,
        idempotency_key: Option<&str>,
    ) -> Result<(PipelineInvokeResult, bool), OrbitError> {
        let job_name = crate::application::job::AGENT_INVOKE_JOB_ID;
        let idempotency_key = idempotency_key
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut input = input;
        if let Some(key) = idempotency_key {
            if let Some(existing) = self.agent_invoke_run_for_key(job_name, key)? {
                return Ok((
                    PipelineInvokeResult {
                        run_id: existing.run_id,
                        job_name: job_name.to_string(),
                        submitted_at: existing.scheduled_at.to_rfc3339(),
                        queued: existing.state == JobRunState::Pending,
                    },
                    true,
                ));
            }
            if let Some(object) = input.as_object_mut() {
                object.insert(
                    AGENT_INVOKE_IDEMPOTENCY_KEY_FIELD.to_string(),
                    Value::String(key.to_string()),
                );
            }
        }
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            trusted_host: true,
            ..PipelineSubmission::catalog(job_name, input.clone(), Some(actor))
        });
        self.record_pipeline_audit(
            "agent.invoke",
            result.as_ref().ok().map(|value| value.run_id.as_str()),
            Some(actor),
            match &result {
                Ok(_) => AuditEventStatus::Success,
                Err(_) => AuditEventStatus::Failure,
            },
            json!({
                "actor": actor,
                "job_name": job_name,
                "run_id": result.as_ref().ok().map(|value| value.run_id.clone()),
                "idempotency_key": idempotency_key,
                "input_hash": input_hash(&input),
            }),
            result.as_ref().err().map(|error| error.to_string()),
        )?;
        result.map(|invoke| (invoke, false))
    }
    /// The newest recent run of `job_name` submitted under `key`, if any.
    fn agent_invoke_run_for_key(
        &self,
        job_name: &str,
        key: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        let runs = self.list_job_runs(crate::application::job::JobRunListParams {
            job_id: Some(job_name.to_string()),
            limit: Some(AGENT_INVOKE_IDEMPOTENCY_SCAN_LIMIT),
            ..Default::default()
        })?;
        Ok(runs.into_iter().find(|run| {
            run.input
                .as_ref()
                .and_then(|input| input.get(AGENT_INVOKE_IDEMPOTENCY_KEY_FIELD))
                .and_then(Value::as_str)
                == Some(key)
        }))
    }
}
