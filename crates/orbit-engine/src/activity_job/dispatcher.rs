use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use orbit_types::workflow::activity_job::V2AuditEventKind;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, DeterministicSpec};

use crate::context::RuntimeHost;
use orbit_common::{OrbitError, RecoverableVcsConflict};
use orbit_tools::{FsAuditLogger, FsCallEvent, FsCallEventKind};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::telemetry::InvocationTrace;
use orbit_types::tool::McpCapability;
use orbit_types::workflow::{DeterministicAction, ExecutorSandboxKind};
use serde_json::Value;
use thiserror::Error;

use super::audit_writer::V2AuditWriter;
use super::cli_runner::run_cli_backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCliExecutor {
    pub command: String,
    pub args: Vec<String>,
}

/// The registered `local_shell` executor definition behind a deterministic
/// shell step, resolved by the host [ORB-11294].
///
/// Every field is the *default* the definition contributes; the activity's own
/// `config` block supplies the step's command and may override the timeout.
/// Sandbox policy is not repeated here — it stays on the single
/// [`RuntimeHost::resolve_executor_sandbox`] boundary the CLI runner already
/// uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedShellExecutor {
    /// Program used when the activity config names no `command`. This is where
    /// a legacy `cli_command` definition's `command` field lands.
    pub command: Option<String>,
    /// Static arguments prepended to the activity's `args`.
    pub args: Vec<String>,
    /// Environment entries the definition adds on top of the host's
    /// `[execution.env]` baseline.
    pub env: BTreeMap<String, String>,
    /// Default wall-clock budget for steps that do not set `timeout_ms`.
    pub timeout_seconds: Option<u64>,
}

/// Open host object backing a Linux runtime convenience grant.
///
/// The descriptor is acquired while the runtime path is validated and remains
/// alive until the sandboxed provider exits. The displayed path is only the
/// namespace destination; it is never reopened as the mount source.
#[derive(Clone, Debug)]
pub struct LinuxRuntimeWriteAuthority {
    pub path: PathBuf,
    pub handle: Arc<File>,
    /// Shared connection that prevents SQLite last-close cleanup from
    /// replacing a descriptor-backed WAL/SHM file set while the provider runs.
    pub wal_file_set_lease: Option<Arc<orbit_common::storage::sqlite::WalFileSetLease>>,
}

impl PartialEq for LinuxRuntimeWriteAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && Arc::ptr_eq(&self.handle, &other.handle)
    }
}

impl Eq for LinuxRuntimeWriteAuthority {}

/// Sandbox descriptor for a CLI invocation. The host resolves the executor's
/// `sandbox` declaration and the activity's `fsProfile` against the active
/// policy and workspace root; the engine compiles the OS-specific payload
/// just before spawn (keeping the orbit-exec dependency local to orbit-engine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSandbox {
    /// Executor choice, including explicit off (no wrapper or profile use).
    pub kind: ExecutorSandboxKind,
    /// Workspace-absolute resolved `read` / `modify` rules from the activity's
    /// `FsProfile`. The engine passes this to `orbit_exec::compile_*_profile`
    /// to produce a kernel-shaped payload. Unused and empty for explicit off.
    pub fs_profile: ResolvedFsProfile,
    /// Whether to fall back to bare exec if the OS primitive is unavailable.
    pub allow_fallback: bool,
    /// Whether the subprocess runs in an Orbit-owned disposable worktree.
    /// Linux may snapshot-expand non-subtree deny globs only in this case.
    pub managed_worktree: bool,
    /// Linux runtime grants whose host objects were opened by the resolving
    /// host. Empty for other backends and ordinary policy-derived grants.
    pub runtime_write_authority: Vec<LinuxRuntimeWriteAuthority>,
}

/// Input bundle for a single v2 activity dispatch.
pub struct V2DispatchInput<'a> {
    pub activity_name: &'a str,
    pub spec: &'a ActivityV2Spec,
    pub fs_profile: Option<&'a str>,
    pub input: Value,
    pub audit: Arc<V2AuditWriter>,
    pub run_id: &'a str,
    /// Runtime host for agent_loop + deterministic paths. A `None` host is only
    /// valid for callers that never dispatch a host-backed activity; host-backed
    /// specs return `DispatchError::HostRequired` when it is absent.
    pub host: Option<&'a dyn RuntimeHost>,
}

/// Outcome of a v2 dispatch attempt.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub success: bool,
    pub output: Value,
    pub message: Option<String>,
    pub invocation: Option<DispatchInvocationTrace>,
}

#[derive(Debug, Clone)]
pub struct DispatchInvocationTrace {
    pub provider: String,
    pub model: Option<String>,
    pub trace: InvocationTrace,
}

#[derive(Debug, Error, Clone)]
pub enum DispatchError {
    #[error("runtime host required for activity type `{0}` but none provided")]
    HostRequired(&'static str),

    #[error("deterministic action not registered: {0}")]
    DeterministicActionNotRegistered(String),

    /// [ORB-10385] A resolved catalog activity names a deterministic action
    /// the executing runtime does not implement — the job/activity assets and
    /// the installed binary are out of sync. Raised by
    /// [`crate::validate_job_deterministic_actions`] before any step runs, so
    /// the run never admits a task or creates a worktree it cannot finish.
    #[error(
        "activity `{activity}` references deterministic action `{action}`, which is not registered in the executing runtime — the loaded catalog asset and the installed orbit binary are out of sync; reinstall or rebuild orbit, or remove the activity from the job"
    )]
    DeterministicActionUnavailable { activity: String, action: String },

    #[error("deterministic action `{action}` failed: {message}")]
    DeterministicActionFailed { action: String, message: String },

    #[error("agent_loop run failed: {0}")]
    AgentLoopFailed(String),

    /// CLI subprocess invocation failed at the host layer (e.g. failed to
    /// spawn, or provider key unknown). Wraps the host's error text verbatim.
    /// Treated as transient: the step retry wrapper may re-attempt it.
    #[error("cli invocation failed: {0}")]
    CliInvocationFailed(String),

    /// CLI subprocess invocation failed in a way retrying cannot fix —
    /// agent config rejected, executable missing, or permission denied
    /// (ORB-10006). Non-retryable: the step retry wrapper fails fast
    /// instead of burning attempts on a deterministic failure.
    #[error("cli invocation failed (permanent): {0}")]
    CliInvocationPermanent(String),

    /// A linked-worktree provider invocation changed the registered primary
    /// checkout. Ordinary retries must not compound or misattribute the delta.
    /// An explicitly configured recovery activity may inspect the diagnostic
    /// once before the executor's single post-recovery attempt (ORB-10306).
    #[error("worktree integrity violation `{code}`: {diagnostic}")]
    WorktreeIntegrity {
        code: &'static str,
        diagnostic: String,
    },

    /// A deterministic VCS action proved that it stopped on actual unmerged
    /// index entries and supplied the pinned base evidence needed for one
    /// bounded repair. This bypasses ordinary retry so the configured conflict
    /// recovery agent is the only additional attempt.
    #[error(
        "recoverable VCS conflict during '{operation}': original base '{original_base_sha}', target base '{target_base_sha}'; {diagnostic}; conflicting paths: {}",
        conflicting_paths.join(", ")
    )]
    RecoverableVcsConflict {
        operation: String,
        original_base_sha: String,
        target_base_sha: String,
        conflicting_paths: Vec<String>,
        diagnostic: String,
    },

    /// Tool-allowlist denial (§6). Non-retryable — the retry wrapper must not
    /// re-attempt a denied call. Phase 2 formerly translated this to
    /// `Ok(terminated)`; Phase 3 surfaces it structurally so the DAG executor
    /// can classify it.
    #[error("tool `{tool_name}` denied at iteration {iteration}")]
    ToolDenied { tool_name: String, iteration: u32 },

    /// A task asked for a tool that cannot enter an agent activity allowlist.
    #[error("task `{task_id}` required tool `{tool_name}` failed admission: {reason}")]
    RequiredToolAdmission {
        task_id: String,
        tool_name: String,
        reason: String,
    },

    /// Job validation rejected the spec at load time.
    #[error("job validation failed: {0}")]
    JobValidation(String),

    /// A step's `retry:` block violates a config invariant (ORB-10006).
    /// Caught by `validate_job` before any step executes; the message names
    /// the offending values.
    #[error("step `{step_id}`: invalid retry config: {field} = {value} violates `{invariant}`")]
    RetryConfigInvalid {
        step_id: String,
        field: &'static str,
        value: u64,
        invariant: String,
    },

    /// Generic job-executor error — distinct from per-activity failures.
    #[error("job executor: {0}")]
    JobExecution(String),

    #[error("audit write failed: {0}")]
    AuditFailed(String),
}

impl DispatchError {
    /// Whether this error should bypass the retry wrapper. Tool denials,
    /// unknown deterministic actions, validation errors, and permanent CLI
    /// invocation failures are non-retryable (§4.3: "Non-retryable errors —
    /// schema violations, allowlist denials, cancellation — skip retry").
    pub fn is_non_retryable(&self) -> bool {
        matches!(
            self,
            DispatchError::ToolDenied { .. }
                | DispatchError::RequiredToolAdmission { .. }
                | DispatchError::DeterministicActionNotRegistered(_)
                | DispatchError::DeterministicActionUnavailable { .. }
                | DispatchError::JobValidation(_)
                | DispatchError::RetryConfigInvalid { .. }
                | DispatchError::HostRequired(_)
                | DispatchError::CliInvocationPermanent(_)
                | DispatchError::WorktreeIntegrity { .. }
                | DispatchError::RecoverableVcsConflict { .. }
        )
    }

    /// Whether an error that bypasses normal retry may still reach an
    /// explicitly configured recovery activity.
    ///
    /// Worktree integrity failures carry the structured checkout diagnostic a
    /// recovery agent needs to establish whether reconciliation is safe. All
    /// other non-retryable classes retain their fail-fast behavior.
    pub fn allows_recovery(&self) -> bool {
        matches!(
            self,
            DispatchError::WorktreeIntegrity { .. } | DispatchError::RecoverableVcsConflict { .. }
        )
    }
}

/// Translate a [`DispatchError`] into the workspace-public [`OrbitError`]
/// surface at crate boundaries.
///
/// Validation failures keep their dedicated [`OrbitError::JobValidation`]
/// variant — including [`DispatchError::DeterministicActionUnavailable`],
/// which is raised by the same pre-execution validation pass [ORB-10385].
/// Everything else collapses into [`OrbitError::InvalidInput`] with the
/// dispatch error's rendered message. Callers translate with
/// `.map_err(dispatch_error_to_orbit)?` per
/// `docs/design-patterns/error_translation.md` [ORB-10013].
pub fn dispatch_error_to_orbit(error: DispatchError) -> OrbitError {
    match error {
        DispatchError::JobValidation(message) => OrbitError::JobValidation(message),
        unavailable @ DispatchError::DeterministicActionUnavailable { .. } => {
            OrbitError::JobValidation(unavailable.to_string())
        }
        DispatchError::RecoverableVcsConflict {
            operation,
            original_base_sha,
            target_base_sha,
            conflicting_paths,
            diagnostic,
        } => OrbitError::RecoverableVcsConflict(Box::new(RecoverableVcsConflict {
            operation,
            original_base_sha,
            target_base_sha,
            conflicting_paths,
            diagnostic,
        })),
        other => OrbitError::InvalidInput(format!("{other}")),
    }
}

/// Dispatch a v2 activity by type. Emits §7 activity.started/finished
/// events around the per-type runner and nests the runner's events beneath.
pub fn dispatch_v2_activity(input: V2DispatchInput<'_>) -> Result<DispatchOutcome, DispatchError> {
    dispatch_v2_activity_inner(input, true)
}

pub(crate) fn dispatch_v2_activity_without_run_id_injection(
    input: V2DispatchInput<'_>,
) -> Result<DispatchOutcome, DispatchError> {
    dispatch_v2_activity_inner(input, false)
}

fn dispatch_v2_activity_inner(
    input: V2DispatchInput<'_>,
    inject_run_id_into_input: bool,
) -> Result<DispatchOutcome, DispatchError> {
    let activity_input = if inject_run_id_into_input {
        inject_run_id(&input.input, input.run_id)
    } else {
        input.input.clone()
    };
    let spec = input.spec;
    let activity_type = match spec {
        ActivityV2Spec::AgentLoop(_) => "agent_loop",
        ActivityV2Spec::Deterministic(_) => "deterministic",
    };

    let activity_event_id = input
        .audit
        .emit(
            orbit_types::workflow::activity_job::V2AuditEventKind::ActivityStarted {
                activity_name: input.activity_name.to_string(),
                activity_type: activity_type.to_string(),
            },
        )
        .map_err(|err| DispatchError::AuditFailed(format!("{err:?}")))?;
    input.audit.push_parent_lossy(activity_event_id);

    let result = match spec {
        ActivityV2Spec::AgentLoop(spec) => match input.host {
            Some(host) => run_agent_loop_activity(
                host,
                input.activity_name,
                spec,
                input.run_id,
                input.audit.clone(),
                &activity_input,
                input.fs_profile,
            ),
            None => Err(DispatchError::HostRequired("agent_loop")),
        },
        ActivityV2Spec::Deterministic(spec) => match input.host {
            Some(host) => run_deterministic(
                host,
                input.run_id,
                input.activity_name,
                spec,
                input.fs_profile,
                input.audit.clone(),
                &activity_input,
            ),
            None => Err(DispatchError::HostRequired("deterministic")),
        },
    };

    input.audit.pop_parent_lossy();
    let outcome_str = match &result {
        Ok(o) if o.success => "success",
        Ok(_) => "failed",
        Err(_) => "error",
    };
    input.audit.emit_lossy(
        orbit_types::workflow::activity_job::V2AuditEventKind::ActivityFinished {
            activity_name: input.activity_name.to_string(),
            outcome: outcome_str.to_string(),
        },
    );

    result
}

pub(crate) fn inject_run_id(input: &Value, run_id: &str) -> Value {
    let Value::Object(map) = input else {
        return input.clone();
    };
    if map.contains_key("run_id") {
        // An explicit `run_id` is a worktree identity token (epic pipelines
        // pin `epic-<task-id>`). The admitted job still owns execution
        // authority; expose it as `job_run_id` when the caller did not.
        if map.contains_key("job_run_id") {
            return input.clone();
        }
        let mut augmented = map.clone();
        augmented.insert("job_run_id".to_string(), Value::String(run_id.to_string()));
        return Value::Object(augmented);
    }

    let mut augmented = map.clone();
    augmented.insert("run_id".to_string(), Value::String(run_id.to_string()));
    Value::Object(augmented)
}

/// Name the dispatching step in a deterministic action's input.
///
/// [ORB-10971] An action that persists something into the run's own state —
/// a child dispatch checkpoint, say — needs to say *which* step produced it,
/// and `run_id` alone cannot. Scoped to the deterministic path: agent-loop
/// envelopes are a provider-facing contract and are deliberately left alone.
/// An input that already carries `step_id` wins, so a job asset can still
/// address a different step explicitly.
fn inject_step_id(input: &Value, step_id: &str) -> Value {
    let Value::Object(map) = input else {
        return input.clone();
    };
    if map.contains_key("step_id") {
        return input.clone();
    }

    let mut augmented = map.clone();
    augmented.insert("step_id".to_string(), Value::String(step_id.to_string()));
    Value::Object(augmented)
}

fn run_deterministic(
    host: &dyn RuntimeHost,
    run_id: &str,
    activity_name: &str,
    spec: &DeterministicSpec,
    fs_profile: Option<&str>,
    audit: Arc<V2AuditWriter>,
    input: &Value,
) -> Result<DispatchOutcome, DispatchError> {
    let mut tool_context = host.tool_context_for_activity(
        Some(run_id),
        fs_profile,
        Some(v2_fs_audit_logger(audit.clone())),
        None,
    );
    tool_context
        .session_context
        .effective_capabilities
        .insert(McpCapability::Runner);
    let output = match DeterministicAction::parse(&spec.action) {
        Some(DeterministicAction::Engine(action)) => {
            let state_context = crate::executor::automation::StateExecutionContext {
                run_id: input
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
                fs_profile: fs_profile.map(ToOwned::to_owned),
                ..crate::executor::automation::StateExecutionContext::default()
            };
            crate::executor::automation::execute_engine_action(
                host,
                action,
                &spec.config,
                input,
                Some(&state_context),
            )
            .map_err(|error| match error {
                OrbitError::RecoverableVcsConflict(conflict) => {
                    DispatchError::RecoverableVcsConflict {
                        operation: conflict.operation,
                        original_base_sha: conflict.original_base_sha,
                        target_base_sha: conflict.target_base_sha,
                        conflicting_paths: conflict.conflicting_paths,
                        diagnostic: conflict.diagnostic,
                    }
                }
                error => DispatchError::DeterministicActionFailed {
                    action: spec.action.clone(),
                    message: error.to_string(),
                },
            })?
        }
        Some(DeterministicAction::Core(_)) | None => host.run_deterministic(
            &spec.action,
            &spec.config,
            &inject_step_id(input, activity_name),
            tool_context,
        )?,
    };
    Ok(DispatchOutcome {
        success: true,
        output,
        message: None,
        invocation: None,
    })
}

fn run_agent_loop_activity(
    host: &dyn RuntimeHost,
    activity_name: &str,
    spec: &AgentLoopSpec,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
) -> Result<DispatchOutcome, DispatchError> {
    run_cli_backend(host, spec, activity_name, run_id, audit, input, fs_profile)
        .map(|outcome| label_failure_with_step(activity_name, outcome))
}

/// [ORB-10449] Prefix a failing CLI agent-loop message with the step that
/// produced it.
///
/// A run surfaces only its terminal message, and the executor's fallback
/// (`step `<id>` completed with success=false`) is used only when the step
/// reports no message at all. So a step that *does* report one was previously
/// anonymous in the run record — an operator saw the symptom without the
/// origin. Naming the step here keeps that fix in one place for every CLI
/// agent-loop failure mode (timeout, nonzero exit, protocol violation,
/// invalid envelope).
fn label_failure_with_step(activity_name: &str, mut outcome: DispatchOutcome) -> DispatchOutcome {
    if !outcome.success
        && let Some(message) = outcome.message.take()
    {
        outcome.message = Some(format!("step `{activity_name}`: {message}"));
    }
    outcome
}

struct V2FsAuditLogger {
    audit: Arc<V2AuditWriter>,
}

impl FsAuditLogger for V2FsAuditLogger {
    fn emit(&self, event: FsCallEvent) -> Result<(), OrbitError> {
        let kind = match event.kind {
            FsCallEventKind::Request => V2AuditEventKind::FsCallRequest {
                profile: event.profile,
                op: event.op,
                path: event.path,
                allowed: event.allowed,
                matched_rule: event.matched_rule,
            },
            FsCallEventKind::Result => V2AuditEventKind::FsCallResult {
                profile: event.profile,
                op: event.op,
                path: event.path,
                allowed: event.allowed,
                matched_rule: event.matched_rule,
            },
            FsCallEventKind::Denied => V2AuditEventKind::FsCallDenied {
                profile: event.profile,
                op: event.op,
                path: event.path,
                allowed: event.allowed,
                matched_rule: event.matched_rule,
            },
        };

        self.audit
            .emit(kind)
            .map(|_| ())
            .map_err(|error| OrbitError::Execution(format!("audit write failed: {error}")))
    }
}

pub(crate) fn v2_fs_audit_logger(audit: Arc<V2AuditWriter>) -> Arc<dyn FsAuditLogger> {
    Arc::new(V2FsAuditLogger { audit })
}
