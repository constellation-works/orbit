use serde::{Deserialize, Serialize};

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
    Friction,
    /// One file stored under a task's artifacts directory.
    Artifact,
    Skill,
    Job,
    JobRun,
    Activity,
    DesignFeature,
    AgentSession,
    Workspace,
}

impl std::fmt::Display for NotFoundKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Tool => "tool",
            Self::Task => "task",
            Self::Friction => "friction record",
            Self::Artifact => "task artifact",
            Self::Skill => "skill",
            Self::Job => "job",
            Self::JobRun => "job run",
            Self::Activity => "activity",
            Self::DesignFeature => "design feature",
            Self::AgentSession => "agent session",
            Self::Workspace => "workspace",
        };
        f.write_str(kind)
    }
}

/// Evidence behind [`crate::OrbitError::DependencyNotDelivered`]: which task was
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

/// One unmerged index path and the conservative trivial-resolution classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictedFile {
    pub path: String,
    /// True only if index modes and object IDs prove identical sides or an unchanged side.
    pub deterministically_resolvable: bool,
}

/// Structured evidence for a merge or rebase conflict that an explicitly
/// configured recovery activity may repair. Keeping the classification typed
/// prevents unrelated VCS diagnostics from launching a recovery agent based
/// on message matching.
#[derive(Debug, Clone, Serialize)]
pub struct RecoverableVcsConflict {
    pub operation: String,
    pub original_base_sha: String,
    pub target_base_sha: String,
    pub conflicting_paths: Vec<String>,
    pub conflicting_files: Vec<ConflictedFile>,
    pub diagnostic: String,
}

/// Evidence behind [`crate::OrbitError::WorkspaceClaimHeld`]: the refused operation,
/// the incumbent holder, its claim id, and the instant the claim lapses.
///
/// The holder's token is deliberately absent. A refusal travels to the
/// contender, and a contender that could read the token out of its own refusal
/// would turn the claim into a formality.
///
/// Boxed into the error enum for the same reason as
/// [`DependencyNotDelivered`]: four inline strings for one rare refusal would
/// widen every `Result<_, OrbitError>` in the workspace.
/// Evidence behind [`crate::OrbitError::FrictionNotLocal`]: an unqualified
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

/// A SQLite bootstrap operation exhausted the connection's busy timeout.
///
/// The native SQLite code is classified before crossing the storage boundary,
/// so detached-worker recovery never depends on matching an error string.
#[derive(Debug, Serialize)]
pub struct SqliteContention {
    pub path: String,
    pub phase: String,
    pub detail: String,
}

/// The error family an [`crate::OrbitError::StorageAccessDenied`] reports as.
///
/// A read-only or access-denied failure keeps the message prefix and the
/// stable error code of the variant it would otherwise have been built as, so
/// carrying the class changes no user-visible text and no wire code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageLayer {
    /// Reported as [`crate::OrbitError::Io`].
    Io,
    /// Reported as [`crate::OrbitError::Store`].
    Store,
    /// Reported as [`crate::OrbitError::Migration`].
    Migration,
}

impl StorageLayer {
    /// The stable error code the CLI and MCP surfaces report for this layer.
    pub fn error_code(self) -> &'static str {
        match self {
            Self::Io => "io_error",
            Self::Store => "store_error",
            Self::Migration => "migration_failed",
        }
    }
}

impl std::fmt::Display for StorageLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Io => "io error",
            Self::Store => "store error",
            Self::Migration => "schema migration failed",
        })
    }
}

/// Why the owner refused a distributed-execution claim or handoff mutation.
///
/// Every kind is reported as the invalid-input refusal it has always been;
/// the kind lets callers decide how to answer it without reading its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimRefusalKind {
    /// The claim acted under is not the owner's current claim, or no longer
    /// is in the phase the action was prepared against.
    StaleClaim,
    /// No claim or accepted handoff with this id is current on the owner.
    NotCurrent,
    /// The candidate offered does not match the one the handoff recorded.
    HandoffCandidateMismatch,
    /// The handoff identity does not match the accepted record.
    HandoffIdentityMismatch,
    /// The repository's validation requirements changed since the handoff.
    ValidationRequirementsChanged,
    /// The landing authority recorded at approval was revoked.
    LandingAuthorityRevoked,
    /// The handoff has already landed.
    HandoffAlreadyLanded,
    /// A merge intent was recorded externally and has not been reconciled.
    UnresolvedMergeIntent,
    /// A merge-intent replay needs reconciliation before it can proceed.
    MergeIntentReplayUnreconciled,
}

impl ClaimRefusalKind {
    /// The refusal text the store has always reported for this kind.
    pub fn message(self) -> &'static str {
        match self {
            Self::StaleClaim => "stale_claim",
            Self::NotCurrent => "claim or handoff is not current on this owner",
            Self::HandoffCandidateMismatch => "handoff candidate mismatch",
            Self::HandoffIdentityMismatch => "handoff identity mismatch",
            Self::ValidationRequirementsChanged => "validation requirements changed",
            Self::LandingAuthorityRevoked => "landing authority revoked",
            Self::HandoffAlreadyLanded => "handoff has already landed",
            Self::UnresolvedMergeIntent => "unresolved external merge intent",
            Self::MergeIntentReplayUnreconciled => "merge intent replay requires reconciliation",
        }
    }
}

/// Stable refusal codes of the operator host registry (`orbit host`, the
/// host file and its consumers). The serialized name is the code callers
/// match on, so each variant is protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRegistryCode {
    /// Both the host file and the legacy destinations file exist.
    HostFileConflict,
    /// The probed `machine_id` is already registered.
    HostExists,
    /// A host name is already used by an entry or the local host.
    HostNameConflict,
    /// A task prefix is already used by an entry or the local host.
    TaskPrefixConflict,
    /// The request names, or the probe answered as, the local host.
    HostIsLocal,
    /// The remote predates host identity in its discovery envelope.
    HostTooOld,
    /// A live probe answered with a different identity than the entry.
    HostIdentityMismatch,
    /// No host matches the given name or machine id.
    UnknownHost,
    /// A local replica checkout or running pull drain depends on the host.
    HostInUse,
    /// A legacy destination row did not answer during migration.
    LegacyHostUnreachable,
    /// A task id's prefix is neither this host's nor a registered host's
    /// [ORB-14449].
    UnknownTaskPrefix,
    /// A server that does not relay was asked, by id only, for a task whose
    /// prefix names a registered remote host.
    TaskPrefixRemote,
    /// The host a task id's prefix names did not answer before the call was
    /// delivered.
    OwnerUnreachable,
}

impl HostRegistryCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostFileConflict => "host_file_conflict",
            Self::HostExists => "host_exists",
            Self::HostNameConflict => "host_name_conflict",
            Self::TaskPrefixConflict => "task_prefix_conflict",
            Self::HostIsLocal => "host_is_local",
            Self::HostTooOld => "host_too_old",
            Self::HostIdentityMismatch => "host_identity_mismatch",
            Self::UnknownHost => "unknown_host",
            Self::HostInUse => "host_in_use",
            Self::LegacyHostUnreachable => "legacy_host_unreachable",
            Self::UnknownTaskPrefix => "unknown_task_prefix",
            Self::TaskPrefixRemote => "task_prefix_remote",
            Self::OwnerUnreachable => "owner_unreachable",
        }
    }
}

impl std::fmt::Display for HostRegistryCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
