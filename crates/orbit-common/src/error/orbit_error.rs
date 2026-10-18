use serde::Serialize;
use thiserror::Error;

use super::{
    ArtifactOrigin, CandidateAbsorbed, ClaimRefusalKind, DependencyNotDelivered, FrictionNotLocal,
    HostRegistryCode, NotFoundKind, PrivateVcsFailure, RecoverableVcsConflict, SqliteContention,
    StorageLayer, WorkspaceClaimHeld,
};

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
    /// A host-registry refusal [ORB-14448]. The message names what to run
    /// next; `code` is the stable spec name every surface reports.
    #[error("{code}: {message}")]
    HostRegistry {
        code: HostRegistryCode,
        message: String,
    },
    /// The owner's pull request contract differs from this executor's build.
    /// Retrying cannot repair wire skew; deploy matching builds and restart.
    #[error("protocol_skew: {0}")]
    ProtocolSkew(String),
    /// A plugin tool or `orbit <ns>` verb was refused because the plugin is
    /// switched off in the workspace the call resolved to, although the host
    /// still has it enabled. The tool never ran; `workspace` names the
    /// workspace for a human reader.
    #[error(
        "plugin '{plugin}' is disabled in workspace '{workspace}'; turn it back on from that \
         workspace with `orbit plugin enable {plugin} --scope workspace`"
    )]
    PluginDisabledInWorkspace { plugin: String, workspace: String },
    /// A workspace toggle tried to switch on a plugin the host has disabled.
    /// A toggle can only narrow host state and never grants permissions, so
    /// the host enable (the consent step) has to come first.
    #[error(
        "plugin '{plugin}' is disabled on this host, and a workspace toggle cannot switch it on; \
         enable it on the host first with `orbit plugin enable {plugin} --grant …`"
    )]
    PluginDisabledOnHost { plugin: String },
    /// A plugin declares `spec.build` and the operator did not consent to
    /// running it. The message carries the build plan the consent would
    /// approve.
    #[error("{0}")]
    PluginBuildConsentRequired(String),
    /// A build was asked for where no operator can consent: a managed run, an
    /// agent sandbox, or a plugin backend.
    #[error("{0}")]
    PluginBuildConsentUnavailable(String),
    /// A plugin's `spec.build` declares a network `fetch` phase on a platform
    /// that does not run one (macOS). Refused before anything runs.
    #[error("{0}")]
    PluginBuildFetchUnsupported(String),
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
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A distributed-execution claim or handoff mutation the owner refused.
    /// Displays and reports exactly as [`Self::InvalidInput`]; `kind` is what
    /// callers branch on, never `message`.
    #[error("invalid input: {message}")]
    ClaimRefused {
        kind: ClaimRefusalKind,
        message: String,
    },
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
    /// Conflict recovery certified that the base already carries the candidate.
    /// Keeps the execution error's display and serialized string for compatibility.
    #[error("execution failed: {0}")]
    #[serde(rename = "Execution", serialize_with = "serialize_absorbed_candidate")]
    CandidateAbsorbed(Box<CandidateAbsorbed>),
    /// An execution step failed because an operation it supervised hit its
    /// deadline. Reads and codes exactly as [`Self::Execution`]; the variant
    /// carries the timeout class so callers never read it from the text.
    #[error("execution failed: {message}")]
    ExecutionTimeout { timeout_ms: u64, message: String },
    /// A private automation VCS operation failed. Displays and serializes
    /// exactly as [`Self::Execution`]; the failure class is decided where the
    /// provider's diagnostic is read, so callers never read it from the text.
    #[error("execution failed: {}", .0.message)]
    #[serde(rename = "Execution", serialize_with = "serialize_private_vcs_failure")]
    PrivateVcs(Box<PrivateVcsFailure>),
    /// A child process exceeded its deadline. The supervisor signalled the
    /// owned process group and reaped the leader; descendants that remained in
    /// that group were signalled with it.
    #[error("process timed out after {timeout_ms}ms: {detail}")]
    ProcessTimeout { timeout_ms: u64, detail: String },
    /// Strict worker containment refused a launch before the worker existed.
    #[error(
        "worker containment required but unavailable: {reason}; enable machine.worker_containment and run under a Linux systemd user manager, or drop --strict-worker-containment / machine.worker_containment_strict"
    )]
    WorkerContainmentUnavailable { reason: String },
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
    /// Cancellation could not confirm teardown of the run's open providers.
    /// No PID means the provider audit trail could not be read. Ownership must
    /// be retained until the evidence is readable and every provider is gone.
    #[error("run cancellation incomplete: provider teardown unconfirmed (pid={pid:?}): {reason}")]
    RunProviderTeardownUnconfirmed { pid: Option<u32>, reason: String },
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
    #[error(
        "store error: SQLite database '{}' remained locked during {}: {}",
        .0.path, .0.phase, .0.detail
    )]
    SqliteContention(Box<SqliteContention>),
    #[error("invalid task status transition: {0}")]
    TaskStatusTransition(String),
    #[error("cannot drop system identity tag '{tag}'; pass --allow-drop-system-tags to override")]
    SystemIdentityTagDropped { tag: String },
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
    /// PR delivery was refused at admission because no Git remote of the
    /// checkout names a network host, so `pr_open` could never find a forge.
    /// Raised before any run, worktree or crew exists; `remotes` describes
    /// what the checkout does have.
    #[error(
        "PR delivery needs a Git remote on a forge host, but {remotes}; nothing was dispatched. \
         Switch this workspace to local delivery with `orbit workspace ship-mode local`, or tag \
         the task `delivery:task_local_pipeline` to deliver it locally"
    )]
    PrForgeRemoteMissing { remotes: String },
    /// Completion cannot overtake the linked implementation run while its
    /// recorded PID and start-time identity still name a running owner.
    #[error(
        "task '{task_id}' cannot move to done while linked run '{run_id}' has a verified-live owner"
    )]
    TaskCompletionLiveRun { task_id: String, run_id: String },
    /// A guarded desktop mutation lost its task content compare-and-set.
    #[error(
        "task '{task_id}' revision conflict; refresh authoritative state and preserve your draft"
    )]
    TaskRevisionConflict { task_id: String },
    /// The request committed, but subsequent task projection or provenance refresh failed.
    /// Adapters must report success and reconcile the original request identity.
    #[error(
        "desktop write for task '{task_id}' was accepted; refresh failed: {reason}; reconcile the same request identity"
    )]
    DesktopWriteAccepted { task_id: String, reason: String },
    /// A resume was refused because the source run's retry lineage already
    /// has a non-terminal run. Raised atomically by the store insert on the
    /// shared resume path, so the CLI, MCP, and HTTP surfaces refuse the
    /// duplicate identically; the payload names the live run so a caller can
    /// open, wait on, or cancel it rather than resume again.
    #[error(
        "job run '{source_run_id}' already has a live resume in its retry lineage ({run_id}); wait for it to finish or cancel it before resuming again"
    )]
    ResumeRunInFlight {
        source_run_id: String,
        run_id: String,
    },
    /// Scratch collection refuses every pending, running, or retrying workspace
    /// job, including a stale owner that has not explicitly been reconciled.
    #[error("cannot collect workspace scratch while job runs are pending, running, or retrying: {}", .run_ids.join(", "))]
    TmpGcActiveRuns { run_ids: Vec<String> },
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
    /// [ORB-11253, ORB-12260] A control update lost a compare-and-set to a
    /// concurrent update of the same control. Distinct from
    /// [`OrbitError::JobValidation`] on purpose: nothing is wrong with the
    /// request, it was simply superseded, so the caller re-reads the current
    /// value and decides again rather than correcting anything.
    #[error("{0}")]
    JobRunControlConflict(String),
    #[error("workspace error: {0}")]
    WorkspaceError(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("schema migration failed: {0}")]
    Migration(String),
    /// A filesystem or SQLite operation failed because its target is mounted
    /// read-only or denies access. Classified from the native error (the
    /// `io::ErrorKind` or errno, or SQLite's result code) where it is
    /// translated, so passive callers that fail open never read message text.
    /// Displays and reports exactly as the `layer` variant it stands in for.
    #[error("{layer}: {message}")]
    StorageAccessDenied {
        layer: StorageLayer,
        message: String,
    },
}

const _: () = assert!(
    std::mem::size_of::<OrbitError>() <= 128,
    "OrbitError exceeds its 128-byte size budget; box multi-field variant payloads"
);

fn serialize_absorbed_candidate<S: serde::Serializer>(
    candidate: &CandidateAbsorbed,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&candidate.to_string())
}

fn serialize_private_vcs_failure<S: serde::Serializer>(
    failure: &PrivateVcsFailure,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&failure.message)
}
