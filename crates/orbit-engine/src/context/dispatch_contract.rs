//! Dispatch contracts owned by the runtime host port, independent of engine flows.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::{OrbitError, RecoverableVcsConflict};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::telemetry::InvocationTrace;
use orbit_types::workflow::ExecutorSandboxKind;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCliExecutor {
    pub command: String,
    pub args: Vec<String>,
    /// The executor's declared arguments for a pinned-source inspection run
    /// (`ExecutorDef::inspection_args`): appended to that run's argv, empty
    /// when `args` already leave the native tools on. `None` declares no
    /// inspection surface, and such a run is refused before launch.
    pub inspection_args: Option<Vec<String>>,
    /// How the executor gets staged images to its model
    /// (`ExecutorDef::image_input`). `None` declares that it cannot take
    /// images, and an image-input run is refused before launch.
    pub image_input: Option<orbit_types::workflow::ExecutorImageInput>,
}

/// The registered `local_shell` executor definition behind a deterministic
/// shell step, resolved by the host [ORB-11294].
///
/// Every field is the *default* the definition contributes; the activity's own
/// `config` block supplies the step's command and may override the timeout.
/// Sandbox policy is not repeated here — it stays on the single
/// `RuntimeHost::resolve_executor_sandbox` boundary the CLI runner already
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
    /// Host directories and files the sandboxed process may neither read nor
    /// write, applied after every other rule of the profile. `None` for
    /// explicit off and for a host that masks nothing.
    pub mask: Option<SandboxMask>,
}

/// Directories and files hidden from a sandboxed process, whatever its
/// profile grants.
///
/// The host that resolves the sandbox creates `sentinel` and every target
/// directory before it returns them. On Linux each target is replaced by the
/// read-only `sentinel` directory and each existing file by `/dev/null`; on
/// macOS the profile denies reads and writes beneath each target and of each
/// file, present or not, and `sentinel` is unused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxMask {
    pub sentinel: PathBuf,
    pub targets: Vec<PathBuf>,
    /// Single files, such as a credential file beside readable siblings.
    pub files: Vec<PathBuf>,
}

/// Outcome of a v2 dispatch attempt.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub success: bool,
    pub output: Value,
    pub message: Option<String>,
    /// The typed step-failure code parsed once at ingestion, when the
    /// failure declared one.
    pub failure_code: Option<orbit_types::workflow::StepFailureCode>,
    pub invocation: Option<DispatchInvocationTrace>,
}

#[derive(Debug, Clone)]
pub struct DispatchInvocationTrace {
    pub agent_execution_id: Option<i64>,
    pub provider: String,
    pub model: Option<String>,
    /// The crew account the invocation ran under, when its crew sets `env`.
    pub account: Option<String>,
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
    /// `validate_job_deterministic_actions` before any step runs, so
    /// the run never admits a task or creates a worktree it cannot finish.
    #[error(
        "activity `{activity}` references deterministic action `{action}`, which is not registered in the executing runtime — the loaded catalog asset and the installed orbit binary are out of sync; reinstall or rebuild orbit, or remove the activity from the job"
    )]
    DeterministicActionUnavailable { activity: String, action: String },

    #[error("deterministic action `{action}` failed: {message}")]
    DeterministicActionFailed { action: String, message: String },

    /// A deterministic action failed because it could not reach the
    /// claimed task's owner. Displays exactly as
    /// [`DispatchError::DeterministicActionFailed`]; the variant carries
    /// the typed step-failure code parsed from the underlying
    /// `OrbitError` at ingestion, so recovery branches on the code
    /// instead of a marker in the message.
    #[error("deterministic action `{action}` failed: {message}")]
    OwnerRouteUnavailable { action: String, message: String },

    /// A command failed after it ran. Its bounded diagnostic is retained
    /// independently of successful checkpoints, so resume still reruns it.
    #[error("deterministic action `local_shell` failed: {message}")]
    LocalShellFailed { message: String, output: Box<Value> },

    /// [ORB-16307] `pr_complete` lost only a read-only completion observation
    /// to a transient provider failure, before any completion mutation.
    /// Displays exactly as [`DispatchError::DeterministicActionFailed`]; final
    /// recovery may resume the unchanged completion once the host
    /// authenticates its prefix.
    #[error("deterministic action `{action}` failed: {message}")]
    CompletionObservationUnavailable { action: String, message: String },

    /// A verified absorbed candidate. `message` retains the diagnostic's
    /// deterministic-action or post-recovery context without losing its facts.
    #[error("{message}")]
    CandidateAbsorbed {
        candidate: Box<orbit_common::CandidateAbsorbed>,
        message: String,
    },

    /// Pull request contracts differ. Retrying or recovery cannot repair the
    /// running binaries, so the drain must end visibly as failed.
    #[error("protocol_skew: {0}")]
    ProtocolSkew(String),

    /// A deterministic action reached a decision rather than a fault — a
    /// settled non-pass review verdict, an exhausted budget, a refused
    /// policy. Repeating the action reaches the same decision, so neither
    /// retry nor a recovery activity runs; the failure handoff owns it.
    #[error("deterministic action `{action}` refused: {message}")]
    DeterministicActionRefused { action: String, message: String },

    /// A [`DispatchError::DeterministicActionRefused`] the review gate raised
    /// for a refusal the failure handoff decides on. The handoff receives
    /// `refusal` as the step's `error_code`; `message` is for readers.
    #[error("deterministic action `{action}` refused: {message}")]
    ReviewGateRefused {
        action: String,
        refusal: orbit_types::workflow::ReviewGateRefusal,
        message: String,
    },

    /// A deliberate delivery refusal, with its shared typed diagnostic code.
    #[error("deterministic action `{action}` refused: {message}")]
    DeliveryRefused {
        action: String,
        refusal: orbit_types::workflow::DeliveryRefusal,
        message: String,
    },

    /// A settled review needs named external evidence. End the delivery run
    /// without retry, recovery, or the failure handoff; receipt queues review.
    #[error("review_awaiting_evidence: named external checks are pending")]
    ReviewEvidenceHold(Box<orbit_types::workflow::ReviewEvidenceHold>),

    /// The forge kept refusing a delivery push for a server-side reason past
    /// the push's own backoff [ORB-14617]. End the run held at this step,
    /// without retry, recovery, or the failure handoff; the clock resumes it.
    #[error("forge_unavailable: the forge refused the push of {} to {} {} times", .0.head_sha, .0.target_ref, .0.attempts)]
    ForgeUnavailableHold(Box<orbit_types::workflow::ForgeUnavailableHold>),

    /// A child run this step waited on was cancelled and none failed
    /// [ORB-15202]. End the run `cancelled`, without retry, recovery, or the
    /// failure handoff: an operator's cancel is not a parent failure.
    #[error("child_cancelled: {message}")]
    ChildRunCancelled { message: String },

    /// An `auto-task:` step is waiting on a condition, an admission hold or
    /// its bound task (ORB-20158). Not a failure: the run parks durably,
    /// without retry, recovery or the failure handoff, and the owner clock
    /// resumes it at `wake_at`.
    #[error("auto_task_parked: {}", .0.reason)]
    AutoTaskParked(Box<orbit_types::workflow::ParkedWait>),

    /// Completion cannot overtake a task's verified-live implementation run.
    #[error(
        "task '{task_id}' cannot move to done while linked run '{run_id}' has a verified-live owner"
    )]
    TaskCompletionLiveRun { task_id: String, run_id: String },

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

    /// A resumed published delivery's deferred footprint was still held by
    /// another task when its wait expired, so the editing agent never
    /// started. Displays exactly as the host's refusal; the variant carries
    /// the typed `footprint_unavailable` code so the run ends held for a
    /// later admission instead of failed, without retry, step or final
    /// recovery, or the failure handoff. The run-stopped refusal from the
    /// same wait is a plain permanent failure, never this variant.
    #[error("execution failed: {0}")]
    FootprintUnavailable(String),

    /// An inspection cannot start: the filesystem profile is not
    /// `reviewer`, the provider has no CLI runtime, or its executor declares
    /// no `inspection_args`. Configuration errors never spend a provider
    /// session.
    #[error("inspection_tools_unavailable: provider `{provider}`: {reason}")]
    InspectionToolsUnavailable { provider: String, reason: String },

    /// An image-input run cannot start: its executor declares no
    /// `spec.image_input`, its crew sets `vision = false`, its `image_input`
    /// request is malformed, or it stages more images or another format than
    /// the declaration admits. Configuration
    /// errors never spend a provider session.
    #[error("image_input_unavailable: executor `{executor}`: {reason}")]
    ImageInputUnavailable { executor: String, reason: String },

    /// A host-owned Git child exceeded its finite budget. The supervisor has
    /// terminated its process group; recovery state must be inspected as-is.
    #[error("git {operation} timed out after {timeout_ms}ms in '{}': {diagnostic}", root.display())]
    GitTimeout {
        operation: String,
        root: std::path::PathBuf,
        timeout_ms: u64,
        diagnostic: String,
    },

    /// A linked-worktree provider invocation changed the registered primary
    /// checkout. Ordinary retries must not compound or misattribute the delta.
    /// An explicitly configured recovery activity may inspect the diagnostic
    /// once before the executor's single post-recovery attempt (ORB-10306).
    #[error("{}", worktree_dispatch_message(code, diagnostic))]
    WorktreeIntegrity {
        code: &'static str,
        diagnostic: String,
    },

    /// A deterministic VCS action proved that it stopped on actual unmerged
    /// index entries and supplied the pinned base evidence needed for one
    /// bounded repair. This bypasses ordinary retry: the configured conflict
    /// recovery agent repairs the stop and the step is retried, again for each
    /// later commit of the same rebase that stops, within a fixed round bound.
    #[error(
        "recoverable VCS conflict during '{}': original base '{}', target base '{}'; {}; conflicting paths: {}",
        .0.operation, .0.original_base_sha, .0.target_base_sha, .0.diagnostic,
        .0.conflicting_paths.join(", ")
    )]
    RecoverableVcsConflict(Box<RecoverableVcsConflict>),

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

    /// A `target: script:<name>` step refused before anything was spawned:
    /// no grant, a stale grant, a request beyond the grant, a changed
    /// interpreter, a follower without a follower grant, a host without the
    /// script profile, an unusable root or an invalid input. `code` is one
    /// of `orbit_types::workflow::activity_job::script_refusal`.
    /// Non-retryable: another attempt meets the same refusal.
    #[error("{code}: step `{step_id}` script `{script}`: {message}; nothing was spawned")]
    ScriptRefused {
        code: &'static str,
        step_id: String,
        script: String,
        message: String,
    },

    /// A script exited 0 but its stdout is not one JSON document within
    /// the cap that passes the manifest's `output_schema_json`. Not
    /// retried: the same bytes give the same answer.
    #[error("script_output_invalid: step `{step_id}` script `{script}`: {message}")]
    ScriptOutputInvalid {
        step_id: String,
        script: String,
        message: String,
    },

    /// A script step's declared output files broke a rule (over a cap, a
    /// symlink, a directory, an unsafe name), could not be stored, or a
    /// resumed run's recorded set no longer matches its producer record.
    /// `code` is a `script_outputs_*` failure code. No partial set is ever
    /// exposed. Not retried: the same `out/` or record gives the same
    /// answer.
    #[error("{code}: step `{step_id}`: {message}; no output file of the step is exposed")]
    ScriptOutputFiles {
        code: &'static str,
        step_id: String,
        message: String,
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

fn worktree_dispatch_message(code: &str, diagnostic: &str) -> String {
    let value = serde_json::from_str(diagnostic).unwrap_or(Value::Null);
    let message = orbit_types::workflow::activity_job::worktree_failure_message(code, &value);
    match stored_output_cite(&value) {
        Some(cite) => format!("{message} ({cite})"),
        None => message.to_string(),
    }
}

/// The provider output blobs a post-run check cited into the diagnostic. The
/// step error names them as plain text so the stored output stays reachable
/// without printing the diagnostic JSON.
fn stored_output_cite(value: &Value) -> Option<String> {
    let stdout = value.get("stdout_blob_ref")?.as_str()?;
    let stderr = value.get("stderr_blob_ref")?.as_str()?;
    if stdout.is_empty() || stderr.is_empty() {
        return None;
    }
    Some(format!(
        "stdout_blob_ref={stdout}, stderr_blob_ref={stderr}"
    ))
}

impl DispatchError {
    /// Structured failure detail, independently redacted before persistence.
    pub fn error_diagnostic(&self) -> Option<Value> {
        match self {
            Self::WorktreeIntegrity { code, diagnostic } => {
                let mut value = serde_json::from_str::<Value>(diagnostic)
                    .unwrap_or_else(|_| serde_json::json!({"reason": diagnostic}));
                if !value.is_object() {
                    value = serde_json::json!({"reason": value});
                }
                if let Some(object) = value.as_object_mut() {
                    object.insert("code".into(), serde_json::json!(code));
                }
                Some(orbit_common::security::redaction::redact_all_json(value))
            }
            _ => None,
        }
    }

    /// A [`DispatchError::DeterministicActionFailed`] for `action`.
    pub fn action_failed(action: &str, message: impl Into<String>) -> Self {
        DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: message.into(),
        }
    }

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
                | DispatchError::FootprintUnavailable(_)
                | DispatchError::InspectionToolsUnavailable { .. }
                | DispatchError::ImageInputUnavailable { .. }
                | DispatchError::GitTimeout { .. }
                | DispatchError::WorktreeIntegrity { .. }
                | DispatchError::RecoverableVcsConflict(_)
                | DispatchError::TaskCompletionLiveRun { .. }
                | DispatchError::DeterministicActionRefused { .. }
                | DispatchError::ReviewGateRefused { .. }
                | DispatchError::DeliveryRefused { .. }
                | DispatchError::ReviewEvidenceHold(_)
                | DispatchError::ForgeUnavailableHold(_)
                | DispatchError::ChildRunCancelled { .. }
                | DispatchError::AutoTaskParked(_)
                | DispatchError::ScriptRefused { .. }
                | DispatchError::ScriptOutputInvalid { .. }
                | DispatchError::ScriptOutputFiles { .. }
                | DispatchError::ProtocolSkew(_)
        )
    }

    /// The `error_code` the job's terminal failure handoff receives for this
    /// step error. A typed error names its own code; a validation-environment,
    /// red-base, agent-blocker, upgrade, owner-signoff or provider failure is
    /// classified from its text; anything else is `pipeline_step_failed`.
    pub fn failure_code(&self) -> &'static str {
        use orbit_types::workflow as wf;
        if matches!(self, DispatchError::OwnerRouteUnavailable { .. }) {
            return wf::OWNER_ROUTE_UNAVAILABLE_ERROR_CODE;
        }
        let text = || self.to_string();
        match self {
            DispatchError::WorktreeIntegrity { code, .. } => code,
            DispatchError::ScriptRefused { code, .. } => code,
            DispatchError::ScriptOutputFiles { code, .. } => code,
            DispatchError::ScriptOutputInvalid { .. } => {
                orbit_types::workflow::activity_job::script_failure::OUTPUT_INVALID
            }
            DispatchError::RecoverableVcsConflict(_) => "recoverable_vcs_conflict",
            DispatchError::TaskCompletionLiveRun { .. } => "task_completion_live_run",
            DispatchError::ImageInputUnavailable { .. } => wf::IMAGE_INPUT_UNAVAILABLE_ERROR_CODE,
            DispatchError::ReviewGateRefused { refusal, .. } => refusal.error_code(),
            DispatchError::DeliveryRefused { refusal, .. } => refusal.error_code(),
            DispatchError::FootprintUnavailable(_) => wf::FOOTPRINT_UNAVAILABLE_ERROR_CODE,
            _ if wf::is_validation_environment_failure(None, Some(&text())) => {
                wf::VALIDATION_ENVIRONMENT_ERROR_CODE
            }
            _ if wf::is_baseline_red_failure(None, Some(&text())) => wf::BASELINE_RED_ERROR_CODE,
            _ if wf::is_task_blocked_by_agent(None, Some(&text())) => {
                wf::TASK_BLOCKED_BY_AGENT_ERROR_CODE
            }
            _ if wf::is_upgrade_pending(None, Some(&text())) => wf::UPGRADE_PENDING_ERROR_CODE,
            _ if wf::failure_classification_text(&text()).contains(wf::STALLED_MARKER) => {
                wf::STALLED_ERROR_CODE
            }
            // [ORB-23956] The typed step code rides on the outcome. This
            // message token is what the failure handoff sees.
            _ if wf::is_owner_signoff_required(None, Some(&text())) => {
                wf::OWNER_SIGNOFF_REQUIRED_ERROR_CODE
            }
            // [ORB-14266] The handoff preserves the candidate for the next run.
            _ => wf::ProviderFailureClass::of(None, Some(&text()))
                .map_or("pipeline_step_failed", wf::ProviderFailureClass::as_str),
        }
    }

    /// The typed step-failure code this error carries, when it was parsed
    /// at ingestion rather than left for text classification.
    pub fn step_failure_code(&self) -> Option<orbit_types::workflow::StepFailureCode> {
        match self {
            DispatchError::OwnerRouteUnavailable { .. } => {
                Some(orbit_types::workflow::StepFailureCode::OwnerRouteUnavailable)
            }
            DispatchError::FootprintUnavailable(_) => {
                Some(orbit_types::workflow::StepFailureCode::FootprintUnavailable)
            }
            _ => None,
        }
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
            DispatchError::WorktreeIntegrity { .. } | DispatchError::RecoverableVcsConflict(_)
        )
    }
}

/// Translate a [`DispatchError`] into the workspace-public [`OrbitError`]
/// surface at crate boundaries.
///
/// Validation failures keep their dedicated [`OrbitError::JobValidation`]
/// variant — including [`DispatchError::DeterministicActionUnavailable`],
/// which is raised by the same pre-execution validation pass [ORB-10385].
/// The live-completion refusal also retains its typed code and run identity.
/// Other errors collapse into [`OrbitError::InvalidInput`] with the dispatch
/// error's rendered message. Callers translate with
/// `.map_err(dispatch_error_to_orbit)?` per
/// `docs/design-patterns/error_translation.md` [ORB-10013].
pub fn dispatch_error_to_orbit(error: DispatchError) -> OrbitError {
    match error {
        DispatchError::ProtocolSkew(message) => OrbitError::ProtocolSkew(message),
        // The typed footprint code survives the boundary so finalization
        // holds the run instead of failing it.
        DispatchError::FootprintUnavailable(message) => OrbitError::FootprintUnavailable(message),
        DispatchError::JobValidation(message) => OrbitError::JobValidation(message),
        unavailable @ DispatchError::DeterministicActionUnavailable { .. } => {
            OrbitError::JobValidation(unavailable.to_string())
        }
        DispatchError::RecoverableVcsConflict(conflict) => {
            OrbitError::RecoverableVcsConflict(conflict)
        }
        DispatchError::TaskCompletionLiveRun { task_id, run_id } => {
            OrbitError::TaskCompletionLiveRun { task_id, run_id }
        }
        DispatchError::DeliveryRefused {
            action,
            refusal,
            message,
        } => OrbitError::DeliveryRefused {
            refusal,
            message: format!("deterministic action `{action}` refused: {message}"),
        },
        // The typed step-failure code survives the boundary as the
        // remote-tool wire code, so the caller's step record keeps it.
        DispatchError::OwnerRouteUnavailable { action, message } => OrbitError::RemoteTool {
            code: orbit_types::workflow::OWNER_ROUTE_UNAVAILABLE_ERROR_CODE.into(),
            payload: serde_json::json!({
                "code": orbit_types::workflow::OWNER_ROUTE_UNAVAILABLE_ERROR_CODE,
                "message": message,
            }),
            message: format!("deterministic action `{action}` failed: {message}"),
        },
        other => OrbitError::InvalidInput(format!("{other}")),
    }
}
