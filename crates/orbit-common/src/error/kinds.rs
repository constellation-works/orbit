use serde::{Deserialize, Serialize};

/// Identifies how the artifact owner was resolved for an artifact-access diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactOriginMode {
    /// The operation used a checkout on this host.
    Local,
    /// The operation addressed an owner through a federated route.
    Federated,
}

/// Checkout identity carried with artifact-access errors so clients can direct the reader to its
/// owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactOrigin {
    /// Whether the owner was reached locally or through federation.
    pub mode: ArtifactOriginMode,
    /// Root of the worktree on the owning host.
    pub worktree_root: String,
    /// Checked-out branch when known. Detached or unresolved branches can be absent.
    pub branch: Option<String>,
}

/// Stable resource class for missing-resource diagnostics and their structured projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotFoundKind {
    /// A registered callable tool.
    Tool,
    /// A task record in the selected workspace.
    Task,
    /// A recorded friction in the selected scope.
    Friction,
    /// One file stored under a task's artifacts directory.
    Artifact,
    /// A skill resolved by the skill registry.
    Skill,
    /// A job definition available to dispatch.
    Job,
    /// A recorded execution of a job.
    JobRun,
    /// A registered workspace or its selected checkout.
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
    /// Task whose dispatch was refused.
    pub task_id: String,
    /// Dependency recorded as done without delivery to the selected base.
    pub dependency_id: String,
    /// Ref from which the requested run would be cut.
    pub base_ref: String,
    /// Commit the dependency-delivery check used for that ref.
    pub base_sha: String,
    /// Diagnostic with any known delivery commits and the recovery step.
    pub detail: String,
}

/// One unmerged index path and the conservative trivial-resolution classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictedFile {
    /// Repository-relative path of the unmerged index entry.
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
    /// VCS operation that left paths unmerged, such as merge or rebase.
    pub operation: String,
    /// Commit the candidate was based on before the attempted operation.
    pub original_base_sha: String,
    /// Commit the recovery operation tried to move the candidate onto.
    pub target_base_sha: String,
    /// Repository-relative paths still unmerged after the operation.
    pub conflicting_paths: Vec<String>,
    /// Per-path evidence distinguishing trivial index resolutions from paths requiring review.
    pub conflicting_files: Vec<ConflictedFile>,
    /// Diagnostic from the operation for the recovery activity to inspect.
    pub diagnostic: String,
}

/// Evidence behind [`crate::OrbitError::FrictionNotLocal`]: an unqualified
/// `resolves` target is missing from this workspace but present in others
/// on the same host [ORB-11078].
///
/// Boxed into the error enum so the multi-field payload does not widen
/// every `Result<_, OrbitError>`.
#[derive(Debug, Serialize)]
pub struct FrictionNotLocal {
    /// Unqualified friction ID the caller asked to resolve.
    pub friction_id: String,
    /// Task carrying the resolves reference.
    pub task_id: String,
    /// Workspace containing that task and in which the reference was sought.
    pub workspace_id: String,
    /// Other workspace names where the friction exists on this host.
    pub found_in: Vec<String>,
}

/// Evidence returned when another actor holds the workspace claim required by an operation.
///
/// The holder's token is deliberately absent because the refusal travels to
/// a contender. The evidence is boxed to preserve the size of
/// [`crate::OrbitError`].
#[derive(Debug, Serialize)]
pub struct WorkspaceClaimHeld {
    /// The governed operation that was refused, e.g. `orbit.workflow.ship`.
    pub operation: String,
    /// The incumbent's actor label.
    pub holder: String,
    /// Public claim identifier, distinct from the authority-bearing token.
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
    /// Database path that failed to open or initialize.
    pub path: String,
    /// Bootstrap operation that exhausted its lock wait.
    pub phase: String,
    /// SQLite diagnostic captured at the failed operation.
    pub detail: String,
}

/// The error family an [`crate::OrbitError::StorageAccessDenied`] or
/// [`crate::OrbitError::StorageLockBusy`] reports as.
///
/// A classified storage failure keeps the message prefix of its original
/// layer. Access denials use `storage_access_denied` on CLI and MCP; lock-busy
/// failures retain this layer's wire code.
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
    /// The stable code for unclassified and lock-busy failures in this layer.
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

/// Which guard refused a write that `--force` (the dashboard's Force)
/// overrides. Callers branch on this kind, never on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForceOverridableKind {
    /// A non-terminal local run still owns the task.
    RunStillOwnsTask,
    /// Delete was asked for a task outside `proposed` or `rejected`.
    DeleteOutsideProposedOrRejected,
}

impl ForceOverridableKind {
    /// The stable token every surface reports for this guard.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunStillOwnsTask => "run_still_owns_task",
            Self::DeleteOutsideProposedOrRejected => "delete_outside_proposed_or_rejected",
        }
    }
}

impl std::fmt::Display for ForceOverridableKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence behind [`crate::OrbitError::ForceOverridable`]: the guard that
/// refused, its human message, and the facts a caller needs to name the
/// guard without parsing the message. Boxed so the rare refusal does not
/// widen every `Result<_, OrbitError>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForceOverridableRefusal {
    /// The guard that refused.
    pub kind: ForceOverridableKind,
    /// The refusal text, without the `invalid input: ` prefix.
    pub message: String,
    /// The owning run, for [`ForceOverridableKind::RunStillOwnsTask`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The owning run's state, for [`ForceOverridableKind::RunStillOwnsTask`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_state: Option<String>,
    /// The task's status, for [`ForceOverridableKind::DeleteOutsideProposedOrRejected`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_status: Option<String>,
}

/// Stable refusal codes of the operator host registry (`orbit host`, the
/// host file and its consumers). The serialized name is the code callers
/// match on, so each variant is protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRegistryCode {
    /// The retired legacy destinations file exists.
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
    /// Return the stable wire token that CLI and remote clients branch on.
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

/// The stable machine code of an [`OrbitError::RemoteTool`]: what a remote
/// owner, plugin backend or broker answered with.
///
/// Named variants are exactly the codes Orbit decides on; any other code is
/// kept verbatim in [`Self::Other`]. Parse a received code once with
/// [`From`], which never yields `Other` for a named spelling, then match the
/// variant. It serializes, displays and debug-prints as the received string,
/// so every wire spelling round-trips byte-for-byte.
///
/// [`OrbitError::RemoteTool`]: super::OrbitError::RemoteTool
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum RemoteToolCode {
    /// The call lost only to a lock-wait deadline; it can succeed unchanged
    /// later.
    LockBusy,
    /// The requested record does not exist on the remote.
    NotFound,
    /// The remote's protocol contract differs from this build's.
    ProtocolSkew,
    /// The call could not reach the plugin broker.
    PluginBrokerUnavailable,
    /// The remote rejected the request's input; nothing was decided.
    InvalidInput,
    /// The remote host's own configuration is invalid; the request was not judged.
    HostConfiguration,
    /// The remote's catalog role refused the call, before any capability
    /// grant was consulted.
    CapabilityRefused,
    /// The caller's capability grant does not cover the call.
    CapabilityDenied,
    /// The remote's policy refused the call.
    PolicyDenied,
    /// Any other code, exactly as received.
    Other(String),
}

impl RemoteToolCode {
    const NAMED: [Self; 9] = [
        Self::LockBusy,
        Self::NotFound,
        Self::ProtocolSkew,
        Self::PluginBrokerUnavailable,
        Self::InvalidInput,
        Self::HostConfiguration,
        Self::CapabilityRefused,
        Self::CapabilityDenied,
        Self::PolicyDenied,
    ];

    /// The wire spelling of this code.
    pub const fn as_str(&self) -> &str {
        match self {
            Self::LockBusy => "lock_busy",
            Self::NotFound => "not_found",
            Self::ProtocolSkew => "protocol_skew",
            Self::PluginBrokerUnavailable => "plugin_broker_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::HostConfiguration => "host_configuration",
            Self::CapabilityRefused => "capability_refused",
            Self::CapabilityDenied => "capability_denied",
            Self::PolicyDenied => "policy_denied",
            Self::Other(code) => code.as_str(),
        }
    }
}

impl From<String> for RemoteToolCode {
    fn from(code: String) -> Self {
        Self::NAMED
            .into_iter()
            .find(|named| named.as_str() == code)
            .unwrap_or(Self::Other(code))
    }
}

impl From<&str> for RemoteToolCode {
    fn from(code: &str) -> Self {
        Self::from(code.to_string())
    }
}

impl std::fmt::Display for RemoteToolCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Debug for RemoteToolCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl Serialize for RemoteToolCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Facts certified when conflict recovery leaves a candidate on its covering base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateAbsorbed {
    /// The recovered branch.
    pub head: String,
    /// The base commit the recovered branch sits on.
    pub base_sha: String,
    /// The base ref that still reaches the covering commit.
    pub base_ref: String,
    /// The commit that already carries the candidate's content.
    pub covering_commit: String,
}

impl std::fmt::Display for CandidateAbsorbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[candidate_absorbed] git_rebase: conflict recovery left '{}' on base \
             '{}' with no candidate commit; covering commit {} on '{}' already carries the \
             candidate's change, so verified no-diff settlement completes the task",
            self.head, self.base_sha, self.covering_commit, self.base_ref
        )
    }
}

/// Why executable-generation admission refused an operation.
///
/// Clock ticks select quiet holds and recorded upgrade refusals by this kind;
/// the accompanying message remains diagnostic only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeAdmissionRefusalKind {
    /// A writer cannot join the still-running foreign generation.
    WritesWhileForeign,
    /// A pending generation switch holds admission, including an expired waiter.
    SwitchPending,
    /// Live participants did not yield for a breaking migration in time.
    BreakingWaiting,
    /// This binary cannot run beside the live participants.
    Incompatible,
    /// Another admission refusal, with no special clock handling.
    Other,
}

/// A typed admission refusal that retains the execution error's wire message.
/// Boxed in [`crate::OrbitError`] to keep its size bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeAdmissionRefusal {
    /// The admission decision, independent of its wording.
    pub kind: UpgradeAdmissionRefusalKind,
    /// The diagnostic and remedy, without the execution error prefix.
    pub message: String,
}

/// Orbit could not read or authenticate the evidence a workflow prerequisite
/// decision needs. Callers branch on this type; `reason` is diagnostic only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowEvidenceUnavailable {
    /// What could not be read or verified.
    pub reason: String,
}

impl std::fmt::Display for WorkflowEvidenceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workflow prerequisite evidence is unavailable ({})",
            self.reason
        )
    }
}

/// A delivery push the forge kept refusing past its retry budget, decided where
/// the push gave up. Callers read the hold from here, never from `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePushHoldFailure {
    /// The push the forge refused.
    pub hold: orbit_types::workflow::ForgeUnavailableHold,
    /// The rendered message, without the `execution failed: ` prefix.
    pub message: String,
}

/// How a failed private automation VCS operation reads, decided once where
/// the provider's diagnostic is read rather than from a rendered message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateVcsFailureClass {
    /// A transient gateway, transport or GraphQL failure that carries no
    /// credential, permission or permanent-HTTP diagnostic, so a failed
    /// read-only observation may be retried later.
    TransientObservation,
    /// A transient failure whose diagnostic also names a credential,
    /// permission or permanent-HTTP refusal. A lookup may retry it; an
    /// observation is not resumable on it.
    Transient,
    /// A read-only completion observation lost to a transient observation
    /// failure before any completion mutation. The host may resume the
    /// unchanged completion only after authenticating its prefix.
    CompletionObservationUnavailable,
    /// Any other failure.
    Other,
}

impl PrivateVcsFailureClass {
    /// Whether the provider failure was transient, whatever completion then
    /// decided about it.
    pub fn is_transient(self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// A private automation VCS operation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateVcsFailure {
    /// The operation that failed.
    pub operation: String,
    /// How the provider's diagnostic reads.
    pub class: PrivateVcsFailureClass,
    /// The rendered message, without the `execution failed: ` prefix.
    pub message: String,
    /// Set only when a provider adapter recognized an authenticated
    /// throttling answer to a read. The class stays as read, so no transient
    /// retry ever repeats it; whether and when the read may repeat is the
    /// caller's policy.
    pub read_throttle: Option<ProviderReadThrottle>,
}

/// What an authenticated provider throttling answer to a read made
/// observable. Every field is optional: an absent field was not observed and
/// must never be invented. None of them carries a credential.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderReadThrottle {
    /// The provider's `Retry-After` wait, in seconds from the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
    /// The provider's rate-limit reset instant, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_epoch_seconds: Option<u64>,
    /// The rate-limit resource the provider named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The throttled account as the provider named it, such as `user ID 1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// The provider's request identifier for the throttled answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Stable decision-request and channel refusal codes shared by all transports.
/// Deserialized where a refusal crosses a process boundary intact (a plugin
/// backend's host route).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRefusalCode {
    /// No registration covers this caller and channel.
    ChannelNotRegistered,
    /// The registration exists but was revoked before submission.
    ChannelRevoked,
    /// The caller's principal is outside the channel's allowed callers.
    CallerNotAllowed,
    /// The transport omitted the sender or supplied one outside the channel's allowlist.
    SenderNotAllowed,
    /// The transport's forwarded authentication results failed the channel's checks.
    AuthenticationFailed,
    /// The supplied raw message failed the required DKIM checks.
    DkimVerificationFailed,
    /// The DKIM verifier could not decide. The same submission may be retried.
    DkimVerificationUnavailable,
    /// Channel policy requires a verifiable raw message.
    DkimVerificationRequired,
    /// The raw message exceeds the verification input limit.
    RawMessageTooLarge,
    /// The token cannot authorize this submission.
    TokenInvalid,
    /// The one-time token was already redeemed.
    TokenConsumed,
    /// The request's answer window has ended.
    RequestExpired,
    /// A newer decision request superseded this one.
    RequestSuperseded,
    /// The proposed answer is outside the request's recorded options.
    AnswerNotAllowed,
    /// The owner could not find the referenced decision request.
    RequestNotFound,
    /// The decision is already resolved or otherwise cannot accept this answer.
    AnswerNotApplicable,
    /// Decision-channel submissions are disabled by configuration.
    DecisionChannelDisabled,
    /// The plugin is unavailable on this host or in this workspace.
    PluginUnavailable,
    /// This runtime does not own coordination writes for the task.
    HostNotOwner,
}

impl DecisionRefusalCode {
    /// The protocol name exposed as the CLI and MCP structured error code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChannelNotRegistered => "channel_not_registered",
            Self::ChannelRevoked => "channel_revoked",
            Self::CallerNotAllowed => "caller_not_allowed",
            Self::SenderNotAllowed => "sender_not_allowed",
            Self::AuthenticationFailed => "authentication_failed",
            Self::DkimVerificationFailed => "dkim_verification_failed",
            Self::DkimVerificationUnavailable => "dkim_verification_unavailable",
            Self::DkimVerificationRequired => "dkim_verification_required",
            Self::RawMessageTooLarge => "raw_message_too_large",
            Self::TokenInvalid => "token_invalid",
            Self::TokenConsumed => "token_consumed",
            Self::RequestExpired => "request_expired",
            Self::RequestSuperseded => "request_superseded",
            Self::AnswerNotAllowed => "answer_not_allowed",
            Self::RequestNotFound => "request_not_found",
            Self::AnswerNotApplicable => "answer_not_applicable",
            Self::DecisionChannelDisabled => "decision_channel_disabled",
            Self::PluginUnavailable => "plugin_unavailable",
            Self::HostNotOwner => "host_not_owner",
        }
    }

    /// Whether the same submission can be retried unchanged after a verifier outage.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::DkimVerificationUnavailable)
    }

    /// Whether this refusal retains the policy-denial audit classification.
    pub fn is_policy_denial(self) -> bool {
        matches!(
            self,
            Self::ChannelNotRegistered
                | Self::ChannelRevoked
                | Self::CallerNotAllowed
                | Self::SenderNotAllowed
                | Self::AuthenticationFailed
                | Self::DkimVerificationFailed
                | Self::DkimVerificationRequired
                | Self::TokenInvalid
                | Self::DecisionChannelDisabled
                | Self::PluginUnavailable
                | Self::HostNotOwner
        )
    }
}

impl std::fmt::Display for DecisionRefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable fleet-notice refusal codes shared by every transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeRefusalCode {
    /// The service could not establish the poster's identity.
    PosterUnidentified,
    /// The poster is outside the authority for the requested notice scope.
    ScopeRefused,
    /// This caller cannot post the requested kind of notice.
    KindRefused,
    /// The post lacks the required evidence or task reference.
    EvidenceRequired,
    /// The requested expiry outlasts the allowed notice lifetime.
    ExpiryExceedsCap,
    /// The requested expiry is too early or is fixed by the notice's authority.
    ExpiryInvalid,
    /// The applicable notice limit has been reached.
    LimitExceeded,
    /// The caller cannot close the existing notice.
    CloseRefused,
    /// No notice matched the requested identity in scope.
    NotFound,
    /// A submitted field failed notice validation.
    InvalidField,
    /// Raised only by owners older than ORB-22808, whose `notice.agent_posting`
    /// switch could refuse agent posts; kept so their refusals still parse.
    AgentPostingDisabled,
}

impl NoticeRefusalCode {
    /// The protocol name exposed as the CLI and MCP structured error code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PosterUnidentified => "notice_poster_unidentified",
            Self::ScopeRefused => "notice_scope_refused",
            Self::KindRefused => "notice_kind_refused",
            Self::EvidenceRequired => "notice_evidence_required",
            Self::ExpiryExceedsCap => "notice_expiry_exceeds_cap",
            Self::ExpiryInvalid => "notice_expiry_invalid",
            Self::LimitExceeded => "notice_limit_exceeded",
            Self::CloseRefused => "notice_close_refused",
            Self::NotFound => "notice_not_found",
            Self::InvalidField => "notice_invalid_field",
            Self::AgentPostingDisabled => "notice_agent_posting_disabled",
        }
    }

    /// Whether the refusal is an authority decision rather than bad input.
    pub fn is_policy_denial(self) -> bool {
        matches!(
            self,
            Self::PosterUnidentified
                | Self::ScopeRefused
                | Self::KindRefused
                | Self::EvidenceRequired
                | Self::ExpiryExceedsCap
                | Self::LimitExceeded
                | Self::CloseRefused
                | Self::AgentPostingDisabled
        )
    }
}

impl std::fmt::Display for NoticeRefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable refusal codes of an explicit task-pilot submission, carried by
/// [`crate::OrbitError::TaskPilotRefused`] and reported unchanged by every
/// transport (CLI, MCP, remote and HTTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPilotRefusalCode {
    /// The task is not `proposed` or `backlog`.
    UnsupportedStatus,
    /// The task is assigned to a human crew.
    HumanCrew,
    /// A live pilot run already holds the task.
    AlreadyRunning,
    /// A live delivery run carries the task.
    DeliveryRunning,
    /// This checkout is a replica; the owner machine pilots.
    ReplicaCheckout,
}

impl TaskPilotRefusalCode {
    /// The protocol name exposed as the structured error code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedStatus => "task_pilot_unsupported_status",
            Self::HumanCrew => "task_pilot_human_crew",
            Self::AlreadyRunning => "task_pilot_already_running",
            Self::DeliveryRunning => "task_pilot_delivery_running",
            Self::ReplicaCheckout => "task_pilot_replica_checkout",
        }
    }
}

impl std::fmt::Display for TaskPilotRefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a source-repair requeue refused the run it would repair, carried by
/// [`crate::OrbitError::SourceRepairRunStateRefused`].
///
/// Both reasons describe the run's state, not the pull request's checks, so
/// requeuing again cannot succeed until the run changes or is discarded. The
/// producer that decides the refusal attaches the reason; consumers never
/// read it from the rendered message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRepairRunStateRefusal {
    /// The delivery is still running, or its record says it ended but its
    /// worker is not conclusively gone.
    StillRunning,
    /// The run is terminal but is not a conclusively stopped failed delivery
    /// or review-decision hold (or, for a pipeline publication, a stopped
    /// cancelled or succeeded delivery).
    NotStoppedFailed,
}

/// A named source-repair authority refusal, carried by
/// [`crate::OrbitError::SourceRepairAuthorityRefused`].
///
/// The producer that decides the refusal attaches the code, so publication
/// diagnostics name it without reading the rendered error. Each code keeps the
/// exact text and error category its untyped predecessor reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRepairAuthorityRefusal {
    /// The immutable publication admission differs from the bound one. Raised
    /// by the engine's publication check; reports as an execution failure.
    PublicationAdmissionChanged,
    /// A selector previously widened by the host was removed.
    HostSelectorRemoved,
    /// The host's recorded selector widening could not be read.
    HostWideningUnreadable,
    /// Rework permission or the policy bound at admission no longer authorizes publication.
    ReworkAuthorityChanged,
    /// Retaining the original inline publication requires its complete stopped lineage.
    InlineLineageMissing,
    /// A newer execution changed after the source was bound.
    SuccessorChanged,
    /// Ownership of the original checkpoint changed before application.
    CheckpointOwnerChanged,
    /// The evidence used to bind the repair source changed before application.
    SourceEvidenceChanged,
    /// The task's authorized intent changed after source binding.
    TaskMeaningChanged,
    /// The bound source, task, scope or reservation no longer agrees with current state.
    BindingChanged,
    /// The task's recorded repository or URL conflicts with the authenticated publication.
    PrAssociationRepositoryConflict,
    /// The task's recorded PR number conflicts with the authenticated publication.
    PrAssociationNumberConflict,
    /// The authenticated publication has no matching PR association on the task.
    PrAssociationMissing,
}

impl SourceRepairAuthorityRefusal {
    /// The static reason retained as a publication refusal's
    /// `authority_refusal` evidence. Persisted: never reword it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::PublicationAdmissionChanged => "publication admission changed",
            Self::HostSelectorRemoved => "host-widened selector was removed",
            Self::HostWideningUnreadable => "unreadable host selector widening",
            Self::ReworkAuthorityChanged => {
                "publication requires unchanged explicit rework authority and current policy"
            }
            Self::InlineLineageMissing => {
                "kept inline publication requires its complete stopped lineage"
            }
            Self::SuccessorChanged => {
                "source, task, scope or reservation changed before source application: newer execution changed"
            }
            Self::CheckpointOwnerChanged => {
                "source, task, scope or reservation changed before source application: original checkpoint owner changed"
            }
            Self::SourceEvidenceChanged => {
                "source, task, scope or reservation changed before source application: source evidence changed"
            }
            Self::TaskMeaningChanged => {
                "source, task, scope or reservation changed before source application: task meaning changed"
            }
            Self::BindingChanged => {
                "source, task, scope or reservation changed before source application"
            }
            Self::PrAssociationRepositoryConflict => {
                "task PR association has a conflicting repository or URL"
            }
            Self::PrAssociationNumberConflict => "task PR association has a conflicting number",
            Self::PrAssociationMissing => {
                "task PR association is missing for the authenticated publication"
            }
        }
    }

    /// Whether this refusal reports as [`crate::OrbitError::Execution`]
    /// rather than [`crate::OrbitError::JobValidation`].
    pub fn reports_as_execution(self) -> bool {
        matches!(self, Self::PublicationAdmissionChanged)
    }

    /// The message of the untyped error this refusal reports as: the reason
    /// itself for an execution failure, otherwise the reason after the
    /// `source repair refused: ` prefix every job-validation refusal carries.
    pub fn message(self) -> String {
        if self.reports_as_execution() {
            self.reason().to_string()
        } else {
            format!("source repair refused: {}", self.reason())
        }
    }
}

/// Evidence behind [`crate::OrbitError::UnknownWorkspaceSelector`]: the
/// selector that matched neither a registered logical workspace nor a local
/// checkout, and — when a bound MCP session refused an explicit override — the
/// bound workspace and the remedy that keeps its filter.
///
/// Boxed into the error enum so adding it stays well within
/// [`crate::OrbitError`]'s 128-byte size budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnknownWorkspaceSelector {
    /// The supplied selector that failed workspace and checkout resolution.
    pub selector: String,
    /// Current session binding and recovery guidance, when the session has a binding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound: Option<BoundWorkspace>,
}

/// The session's bound workspace, as an unknown explicit selector's refusal
/// names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundWorkspace {
    /// Name of the workspace already bound to this session.
    pub name: String,
    /// How to retry while preserving that workspace as the query filter.
    pub remedy: BoundWorkspaceRemedy,
}

/// How a retry keeps the bound workspace as its filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BoundWorkspaceRemedy {
    /// Omitting `workspace` resolves through the session binding, so omitting
    /// it keeps the bound filter.
    OmitSelector,
    /// Omitting `workspace` on an ID-resolved tool looks the task up in
    /// whichever workspace owns it, which drops the binding. The bound filter
    /// survives only when the bound workspace is passed explicitly.
    PassBoundSelector,
}

impl UnknownWorkspaceSelector {
    /// The human-readable diagnostic without the `invalid input: ` enum prefix.
    pub fn message(&self) -> String {
        let base = format!(
            "unknown workspace selector '{}'; run `orbit workspace list` to see registered workspaces, then pass a registered workspace name, a logical workspace ID, or an absolute local checkout path",
            self.selector
        );
        match &self.bound {
            None => base,
            Some(BoundWorkspace {
                name,
                remedy: BoundWorkspaceRemedy::OmitSelector,
            }) => format!("{base}; this session is bound to workspace '{name}'; omit `workspace`"),
            Some(BoundWorkspace {
                name,
                remedy: BoundWorkspaceRemedy::PassBoundSelector,
            }) => format!(
                "{base}; this session is bound to workspace '{name}'; pass `workspace` as '{name}' to keep that filter, since omitting `workspace` looks the task up by its ID in whichever workspace owns it"
            ),
        }
    }
}
