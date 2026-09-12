use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactOriginMode {
    Local,
    Federated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactOrigin {
    pub mode: ArtifactOriginMode,
    pub worktree_root: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotFoundKind {
    Tool,
    Task,
    /// One file stored under a task's artifacts directory.
    Artifact,
    Skill,
    Job,
    JobRun,
    Activity,
    Adr,
    DesignFeature,
    AgentSession,
    Workspace,
}

impl std::fmt::Display for NotFoundKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Tool => "tool",
            Self::Task => "task",
            Self::Artifact => "task artifact",
            Self::Skill => "skill",
            Self::Job => "job",
            Self::JobRun => "job run",
            Self::Activity => "activity",
            Self::Adr => "ADR",
            Self::DesignFeature => "design feature",
            Self::AgentSession => "agent session",
            Self::Workspace => "workspace",
        };
        f.write_str(kind)
    }
}

/// Evidence behind [`OrbitError::DependencyNotDelivered`]: which task was
/// refused, which of its done dependencies is missing from the base, the base
/// itself, and `detail` — the delivery commits found elsewhere in the
/// repository plus the remedy.
///
/// Boxed into the error enum: five inline strings would widen every
/// `Result<_, OrbitError>` in the workspace past the large-error threshold for
/// one rare refusal.
#[derive(Debug, Serialize)]
pub struct DependencyNotDelivered {
    pub task_id: String,
    pub dependency_id: String,
    pub base_ref: String,
    pub base_sha: String,
    pub detail: String,
}

/// Structured evidence for a merge or rebase conflict that an explicitly
/// configured recovery activity may repair. Keeping the classification typed
/// prevents unrelated VCS diagnostics from launching a recovery agent based
/// on message matching.
#[derive(Debug, Serialize)]
pub struct RecoverableVcsConflict {
    pub operation: String,
    pub original_base_sha: String,
    pub target_base_sha: String,
    pub conflicting_paths: Vec<String>,
    pub diagnostic: String,
}

/// Evidence behind [`OrbitError::WorkspaceClaimHeld`]: the refused operation,
/// the incumbent holder, its claim id, and the instant the claim lapses.
///
/// The holder's token is deliberately absent. A refusal travels to the
/// contender, and a contender that could read the token out of its own refusal
/// would turn the claim into a formality.
///
/// Boxed into the error enum for the same reason as
/// [`DependencyNotDelivered`]: four inline strings for one rare refusal would
/// widen every `Result<_, OrbitError>` in the workspace.
/// Evidence behind [`OrbitError::FrictionNotLocal`]: an unqualified
/// `resolves` target is missing from this workspace but present in others
/// on the same host [ORB-11078].
///
/// Boxed into the error enum so the multi-field payload does not widen
/// every `Result<_, OrbitError>`.
#[derive(Debug, Serialize)]
pub struct FrictionNotLocal {
    pub friction_id: String,
    pub task_id: String,
    pub workspace_id: String,
    pub found_in: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceClaimHeld {
    /// The governed operation that was refused, e.g. `orbit.workflow.ship`.
    pub operation: String,
    /// The incumbent's actor label.
    pub holder: String,
    pub claim_id: String,
    /// RFC 3339 instant at which the claim stops blocking on its own.
    pub expires_at: String,
}

#[derive(Debug, Error, Serialize)]
#[non_exhaustive]
/// Keep this widely propagated error below its 128-byte size budget. Box the
/// payload of any future multi-field variant so adding it does not widen every
/// `Result<_, OrbitError>` in the workspace.
pub enum OrbitError {
    #[error("policy denied: {0}")]
    PolicyDenied(String),
    #[error("{kind} not found: {id}")]
    NotFound { kind: NotFoundKind, id: String },
    /// A governed operation was refused because the caller lacked the required
    /// capability [ORB-10453]. Distinct from [`Self::PolicyDenied`], which is
    /// `orbit-policy`'s filesystem-scoping refusal: this one is about *who is
    /// asking*, not *which path was touched*, and every surface maps it to a
    /// `denied` audit status so refusals stay separable from failures.
    #[error("capability denied: {0}")]
    CapabilityDenied(String),
    #[error("unknown federated workspace selector: {0}")]
    UnknownSelector(String),
    #[error("ambiguous federated destination: {0}")]
    AmbiguousDestination(String),
    /// A destination's callers file names one caller twice, so the grant it
    /// would serve that caller is undecidable. Fails the whole file closed at
    /// load, mirroring [`Self::AmbiguousDestination`] [ORB-11052].
    #[error("ambiguous MCP caller: {0}")]
    AmbiguousCaller(String),
    /// The authenticated key a destination observed does not match the one its
    /// callers file pins that caller to, so the session is refused at
    /// establishment rather than served at a lower ceiling [ORB-11053]. A
    /// silent downgrade would make a key mismatch indistinguishable from a
    /// caller that simply holds a smaller grant.
    #[error("unauthorized MCP caller: {0}")]
    UnauthorizedCaller(String),
    #[error("federated destination unreachable: {0}")]
    UnreachableDestination(String),
    #[error("stale federated route: {0}")]
    StaleRoute(String),
    #[error("unhealthy federated checkout: {0}")]
    UnhealthyCheckout(String),
    #[error("tool is not available on this host: {0}")]
    ToolNotOnThisHost(String),
    #[error("destination capability refused: {0}")]
    CapabilityRefused(String),
    #[error("Invalid ADR status transition: {0}")]
    AdrInvalidTransition(String),
    #[error("{kind} artifact unavailable for {id}")]
    RemoteArtifactUnavailable {
        kind: NotFoundKind,
        id: String,
        artifact_origin: ArtifactOrigin,
    },
    #[error("{kind} artifact is not local to the current worktree: {id}")]
    ArtifactNotLocal {
        kind: NotFoundKind,
        id: String,
        artifact_origin: ArtifactOrigin,
    },
    /// An unqualified `resolves` friction ID belongs to another workspace
    /// on this host, so completing the task must not count as coverage
    /// [ORB-11078]. Distinct from a dangling miss, which is audit-visible
    /// and does not block completion.
    #[error(
        "resolves target '{}' is not in workspace '{}' (found in {}); auto-resolve is workspace-local — resolve it from its owning workspace with `orbit friction resolve <id>` (operator CLI) or `orbit tool run orbit.friction.update` with status resolved (agent path), or land a covering task there",
        .0.friction_id, .0.workspace_id, .0.found_in.join(", ")
    )]
    FrictionNotLocal(Box<FrictionNotLocal>),
    #[error("companion not installed: {0}")]
    CompanionNotInstalled(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("sensitive input rejected for `{field}`: {reason}")]
    SensitiveInput { field: String, reason: String },
    #[error("invalid input: {message}")]
    InvalidInputDiagnostic {
        message: String,
        did_you_mean: Vec<String>,
    },
    #[error("skill validation failed: {0}")]
    SkillValidation(String),
    #[error("job validation failed: {0}")]
    JobValidation(String),
    #[error("agent protocol violation: {0}")]
    AgentProtocolViolation(String),
    #[error("unsupported agent provider: {0}")]
    UnsupportedAgentProvider(String),
    #[error("owner unavailable: {0}")]
    OwnerUnavailable(String),
    #[error("owner negotiation failed: {0}")]
    OwnerNegotiation(String),
    #[error("owner call outcome unknown for {mcp_call_id}: {message}")]
    OutcomeUnknown {
        mcp_call_id: String,
        message: String,
    },
    #[error("remote tool failed ({code}): {message}")]
    RemoteTool {
        code: String,
        message: String,
        payload: serde_json::Value,
    },
    #[error("execution failed: {0}")]
    Execution(String),
    #[error(
        "recoverable VCS conflict during '{}': original base '{}', target base '{}'; {}; conflicting paths: {}",
        .0.operation,
        .0.original_base_sha,
        .0.target_base_sha,
        .0.diagnostic,
        .0.conflicting_paths.join(", ")
    )]
    RecoverableVcsConflict(Box<RecoverableVcsConflict>),
    #[error(
        "run cancellation incomplete: pid={pid}, pgid={pgid:?}, term_sent={term_sent}, kill_sent={kill_sent}, leader_alive={leader_alive}, group_alive={group_alive}"
    )]
    RunCancellationIncomplete {
        pid: u32,
        pgid: Option<i32>,
        term_sent: bool,
        kill_sent: bool,
        leader_alive: bool,
        group_alive: bool,
    },
    #[error("task bundle corrupt for {task_id} at {path}: {reason}")]
    TaskBundleCorrupt {
        task_id: String,
        path: String,
        reason: String,
    },
    /// A bounded advisory file-lock acquisition exhausted its deadline.
    /// Structured lock and holder fields let callers diagnose contention
    /// without parsing the display message.
    #[error("{0}")]
    FileLockTimeout(Box<crate::fs::io::FileLockTimeout>),
    #[error("store error: {0}")]
    Store(String),
    #[error("invalid task status transition: {0}")]
    TaskStatusTransition(String),
    /// A workflow run was refused because a dependency that reached `done` has
    /// not been delivered into the base the run would be cut from
    /// [ORB-10464]. Distinct from [`Self::TaskStatusTransition`]: the
    /// lifecycle transition is legal, the *baseline* is wrong.
    #[error(
        "task '{}' depends on '{}', which is done but not delivered into base '{}' ({}): {}",
        .0.task_id, .0.dependency_id, .0.base_ref, .0.base_sha, .0.detail
    )]
    DependencyNotDelivered(Box<DependencyNotDelivered>),
    /// A ship submission naming explicit task ids was refused because one of
    /// them is already carried by a non-terminal run [ORB-10544]. Raised by the
    /// shared submission path, so every dispatch surface refuses the duplicate
    /// identically; the payload names the contended task and the run that holds
    /// it so a caller can wait on or cancel that run rather than re-dispatch.
    #[error(
        "task {task_id} already has an in-flight run ({run_id}); wait for it to finish or cancel it"
    )]
    ShipRunInFlight { task_id: String, run_id: String },
    /// A governed workflow operation was refused because another operator holds
    /// the exclusive workspace claim [ADR-0352, ORB-10709]. Raised by the shared
    /// run-submission path, so the refusal is identical on every surface, and
    /// the payload names the incumbent and the expiry instant so a caller can
    /// wait it out, ask the holder, or force-release rather than retry blindly.
    /// Contention rejects: never a silent queue, never a silent steal.
    #[error(
        "workspace claim is held by {} until {} ({}); {} requires that claim's token — present it as `claim_token`, wait for the claim to expire, or force-release it",
        .0.holder, .0.expires_at, .0.claim_id, .0.operation
    )]
    WorkspaceClaimHeld(Box<WorkspaceClaimHeld>),
    #[error("invalid job run state transition: {0}")]
    JobRunStateTransition(String),
    /// [ORB-10965] A duplicate `Start` was applied to a job run that a
    /// *different* process already owns. Distinct from
    /// [`OrbitError::JobRunStateTransition`] on purpose: at-least-once delivery
    /// makes this an expected race whose loser must yield to the incumbent,
    /// not a state-machine violation, so callers can tell the two apart
    /// without matching on message text.
    #[error("job run start conflict: {0}")]
    JobRunStartConflict(String),
    /// [ORB-11253] A run-control update lost a compare-and-set to a concurrent
    /// update of the same control. Distinct from
    /// [`OrbitError::JobValidation`] on purpose: nothing is wrong with the
    /// request, it was simply superseded, so the caller re-reads the current
    /// value and decides again rather than correcting anything.
    #[error("job run control conflict: {0}")]
    JobRunControlConflict(String),
    #[error("workspace error: {0}")]
    WorkspaceError(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("schema migration failed: {0}")]
    Migration(String),
}

const _: () = assert!(
    std::mem::size_of::<OrbitError>() <= 128,
    "OrbitError exceeds its 128-byte size budget; box multi-field variant payloads"
);

impl OrbitError {
    pub fn not_found(kind: NotFoundKind, id: impl Into<String>) -> Self {
        Self::NotFound {
            kind,
            id: id.into(),
        }
    }

    pub fn invalid_input_with_suggestions(
        message: impl Into<String>,
        did_you_mean: Vec<String>,
    ) -> Self {
        if did_you_mean.is_empty() {
            Self::InvalidInput(message.into())
        } else {
            Self::InvalidInputDiagnostic {
                message: message.into(),
                did_you_mean,
            }
        }
    }

    pub fn remote_artifact_unavailable(
        kind: NotFoundKind,
        id: impl Into<String>,
        artifact_origin: ArtifactOrigin,
    ) -> Self {
        Self::RemoteArtifactUnavailable {
            kind,
            id: id.into(),
            artifact_origin,
        }
    }

    pub fn artifact_not_local(
        kind: NotFoundKind,
        id: impl Into<String>,
        artifact_origin: ArtifactOrigin,
    ) -> Self {
        Self::ArtifactNotLocal {
            kind,
            id: id.into(),
            artifact_origin,
        }
    }

    pub fn friction_not_local(
        friction_id: impl Into<String>,
        task_id: impl Into<String>,
        workspace_id: impl Into<String>,
        found_in: Vec<String>,
    ) -> Self {
        Self::FrictionNotLocal(Box::new(FrictionNotLocal {
            friction_id: friction_id.into(),
            task_id: task_id.into(),
            workspace_id: workspace_id.into(),
            found_in,
        }))
    }

    pub fn friction_not_local_details(&self) -> Option<&FrictionNotLocal> {
        match self {
            Self::FrictionNotLocal(details) => Some(details),
            _ => None,
        }
    }

    pub fn artifact_origin(&self) -> Option<&ArtifactOrigin> {
        match self {
            Self::RemoteArtifactUnavailable {
                artifact_origin, ..
            }
            | Self::ArtifactNotLocal {
                artifact_origin, ..
            } => Some(artifact_origin),
            _ => None,
        }
    }

    pub fn did_you_mean(&self) -> Option<&[String]> {
        match self {
            Self::InvalidInputDiagnostic { did_you_mean, .. } if !did_you_mean.is_empty() => {
                Some(did_you_mean)
            }
            _ => None,
        }
    }

    /// The contended `(task_id, run_id)` of a ship duplicate-dispatch refusal,
    /// so projections (HTTP 409 body, MCP structured error) can name both
    /// without re-parsing the message.
    pub fn ship_run_in_flight(&self) -> Option<(&str, &str)> {
        match self {
            Self::ShipRunInFlight { task_id, run_id } => Some((task_id, run_id)),
            _ => None,
        }
    }

    /// The refused operation, incumbent holder, claim id, and expiry of a
    /// workspace-claim refusal, so projections (HTTP 409 body, MCP structured
    /// error) can name them without re-parsing the message [ORB-10709].
    pub fn workspace_claim_held(&self) -> Option<&WorkspaceClaimHeld> {
        match self {
            Self::WorkspaceClaimHeld(claim) => Some(claim),
            _ => None,
        }
    }

    pub fn task_bundle_corruption(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::TaskBundleCorrupt {
                task_id,
                path,
                reason,
            } => Some((task_id, path, reason)),
            _ => None,
        }
    }

    pub fn file_lock_timeout(&self) -> Option<&crate::fs::io::FileLockTimeout> {
        match self {
            Self::FileLockTimeout(timeout) => Some(timeout),
            _ => None,
        }
    }

    /// Translate a filesystem write failure.
    ///
    /// EROFS/EACCES name `path` and hint that this is likely a sandbox or
    /// environment mount, not a store defect — the outer-sandbox counterpart
    /// to `linux_bwrap_write_grant_diagnostic`. Other I/O stays a bare
    /// [`Self::Io`] so read and capacity failures keep their existing text.
    pub fn from_write_io(path: &Path, err: std::io::Error) -> Self {
        Self::Io(
            crate::fs::io::write_access_error_message(path, &err)
                .unwrap_or_else(|| err.to_string()),
        )
    }

    /// Whether an operation failed because its persistence target is mounted
    /// read-only or denies writes.
    ///
    /// Filesystem and SQLite adapters currently translate their native errors
    /// at different crate boundaries. Keep the recognition here so passive
    /// bootstrap, cache, and telemetry callers do not each grow a partial list
    /// of platform and SQLite spellings.
    pub fn is_readonly_or_access_failure(&self) -> bool {
        let message = self.to_string().to_ascii_lowercase();
        message.contains("read-only file system")
            || message.contains("readonly filesystem")
            || message.contains("permission denied")
            || message.contains("attempt to write a readonly database")
            || message.contains("database is read-only")
            || message.contains("database is readonly")
            || message.contains(" is not writable:")
    }
}

impl From<std::io::Error> for OrbitError {
    fn from(err: std::io::Error) -> Self {
        if let Some(timeout) = err
            .get_ref()
            .and_then(|source| source.downcast_ref::<crate::fs::io::FileLockTimeout>())
        {
            return OrbitError::FileLockTimeout(Box::new(timeout.clone()));
        }
        OrbitError::Io(err.to_string())
    }
}

use orbit_types::identity::IdentityError;
use orbit_types::policy::PolicyError;
use orbit_types::record::RecordError;
use orbit_types::resource::ResourceError;
use orbit_types::task::TaskError;
use orbit_types::tool::ToolError;
use orbit_types::workflow::WorkflowError;

impl From<IdentityError> for OrbitError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::Invalid(message) => Self::InvalidInput(message),
            IdentityError::InvalidWithSuggestions {
                message,
                did_you_mean,
            } => Self::invalid_input_with_suggestions(message, did_you_mean),
        }
    }
}

impl From<TaskError> for OrbitError {
    fn from(error: TaskError) -> Self {
        match error {
            TaskError::Invalid(message) => Self::InvalidInput(message),
            TaskError::StatusTransition(message) => Self::TaskStatusTransition(message),
        }
    }
}

impl From<WorkflowError> for OrbitError {
    fn from(error: WorkflowError) -> Self {
        match error {
            WorkflowError::Invalid(message) => Self::InvalidInput(message),
            WorkflowError::JobValidation(message) => Self::JobValidation(message),
            WorkflowError::SkillValidation(message) => Self::SkillValidation(message),
        }
    }
}

impl From<PolicyError> for OrbitError {
    fn from(error: PolicyError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<ResourceError> for OrbitError {
    fn from(error: ResourceError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<ToolError> for OrbitError {
    fn from(error: ToolError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<RecordError> for OrbitError {
    fn from(error: RecordError) -> Self {
        match error {
            RecordError::Invalid(message) => Self::InvalidInput(message),
            RecordError::InvalidTransition(message) => Self::AdrInvalidTransition(message),
        }
    }
}

#[cfg(test)]
#[path = "tests/error.rs"]
mod tests;
