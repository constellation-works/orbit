use serde::Serialize;
use thiserror::Error;

use super::{
    ArtifactOrigin, CandidateAbsorbed, ClaimRefusalKind, DecisionRefusalCode,
    DependencyNotDelivered, ForceOverridableRefusal, ForgePushHoldFailure, FrictionNotLocal,
    HostRegistryCode, NotFoundKind, NoticeRefusalCode, PrivateVcsFailure, RecoverableVcsConflict,
    RemoteToolCode, SourceRepairAuthorityRefusal, SourceRepairRunStateRefusal, SqliteContention,
    StorageLayer, TaskPilotRefusalCode, UnknownWorkspaceSelector, UpgradeAdmissionRefusal,
    WorkflowEvidenceUnavailable, WorkspaceClaimHeld,
};

#[derive(Debug, Error, Serialize)]
#[non_exhaustive]
/// Keep this widely propagated error below its 128-byte size budget. Box the
/// payload of any future multi-field variant so adding it does not widen every
/// `Result<_, OrbitError>` in the workspace.
pub enum OrbitError {
    /// Filesystem-scoping policy refused the requested operation.
    #[error("policy denied: {0}")]
    PolicyDenied(String),
    /// A plugin callback selected a checkout its backend sandbox does not cover.
    #[error("{0}")]
    CallbackWorkspaceOutsideSandbox(String),
    /// Resource resolution completed without a match in the selected scope.
    #[error(
        "{message}",
        message = not_found_message(*kind, id, guidance.as_deref())
    )]
    NotFound {
        /// Resource class used by readers to interpret the identifier.
        kind: NotFoundKind,
        /// Identifier supplied to the lookup.
        id: String,
        /// Optional next step displayed after the missing-resource diagnostic.
        #[serde(skip_serializing_if = "Option::is_none")]
        guidance: Option<String>,
    },
    /// A run read omitted the ID, and the workspace has no runs to select.
    #[error("no job runs exist in this workspace; see `orbit run history` for recorded runs")]
    NoJobRuns,
    /// A governed operation was refused because the caller lacked the required
    /// capability [ORB-10453]. Distinct from [`Self::PolicyDenied`], which is
    /// `orbit-policy`'s filesystem-scoping refusal: this one is about *who is
    /// asking*, not *which path was touched*, and every surface maps it to a
    /// `denied` audit status so refusals stay separable from failures.
    #[error("capability denied: {0}")]
    CapabilityDenied(String),
    /// No registered workspace matched the federated selector.
    #[error("unknown federated workspace selector: {0}")]
    UnknownSelector(String),
    /// The destination selector matched multiple federated workspaces or hosts.
    #[error("ambiguous federated destination: {0}")]
    AmbiguousDestination(String),
    /// The destination could not be reached through its registered route.
    #[error("federated destination unreachable: {0}")]
    UnreachableDestination(String),
    /// The saved route no longer represents the destination's current identity or configuration.
    #[error("stale federated route: {0}")]
    StaleRoute(String),
    /// The selected remote checkout cannot serve the operation in its current state.
    #[error("unhealthy federated checkout: {0}")]
    UnhealthyCheckout(String),
    /// The serving host does not offer the requested tool.
    #[error("tool is not available on this host: {0}")]
    ToolNotOnThisHost(String),
    /// The destination refused authority for the routed operation.
    #[error("destination capability refused: {0}")]
    CapabilityRefused(String),
    /// A host-registry refusal [ORB-14448]. The message names what to run
    /// next; `code` is the stable spec name every surface reports.
    #[error("{code}: {message}")]
    HostRegistry {
        /// Stable refusal token used by callers and transports.
        code: HostRegistryCode,
        /// Reader guidance supplied by the host-registry operation.
        message: String,
    },
    /// A decision refusal with a stable code, independent of its human explanation.
    #[error("{code}: {message}")]
    DecisionRefused {
        /// Stable decision refusal classification for transport and audit.
        code: DecisionRefusalCode,
        /// Reader-facing explanation of the decision refusal.
        message: String,
    },
    /// A fleet-notice refusal with a stable code. Nothing was written; the
    /// refusal itself is audited by the notice service.
    #[error("{code}: {message}")]
    NoticeRefused {
        /// Stable notice refusal classification for transport and audit.
        code: NoticeRefusalCode,
        /// Reader-facing explanation of the notice refusal.
        message: String,
    },
    /// An explicit task-pilot submission refused before any run existed.
    /// `code` is the stable token every surface reports; nothing was submitted.
    #[error("{code}: {message}")]
    TaskPilotRefused {
        /// Stable admission code that names the rejected prerequisite.
        code: TaskPilotRefusalCode,
        /// Reader-facing guidance from the admission check.
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
    PluginDisabledInWorkspace {
        /// Identifier of the plugin enabled at host scope.
        plugin: String,
        /// Selected workspace whose override disabled the plugin.
        workspace: String,
    },
    /// A workspace toggle tried to switch on a plugin the host has disabled.
    /// A toggle can only narrow host state and never grants permissions, so
    /// the host enable (the consent step) has to come first.
    #[error(
        "plugin '{plugin}' is disabled on this host, and a workspace toggle cannot switch it on; \
         enable it on the host first with `orbit plugin enable {plugin} --grant …`"
    )]
    PluginDisabledOnHost {
        /// Identifier of the plugin that needs host-level consent.
        plugin: String,
    },
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
    /// The artifact could not be supplied through the owner's federated route.
    #[error("{kind} artifact unavailable for {id}")]
    RemoteArtifactUnavailable {
        /// Resource class that owns the missing artifact.
        kind: NotFoundKind,
        /// Identifier used to read the artifact.
        id: String,
        /// Checkout evidence retained for diagnosing the failed read.
        artifact_origin: ArtifactOrigin,
    },
    /// The artifact is recorded in a different checkout than the one serving this read.
    #[error("{kind} artifact is not local to the current worktree: {id}")]
    ArtifactNotLocal {
        /// Resource class that owns the nonlocal artifact.
        kind: NotFoundKind,
        /// Identifier used to read the artifact.
        id: String,
        /// Owning checkout information for a follow-up read.
        artifact_origin: ArtifactOrigin,
    },
    /// An unqualified `resolves` friction ID belongs to another workspace
    /// on this host, so completing the task must not count as coverage
    /// [ORB-11078]. Distinct from a dangling miss, which is audit-visible
    /// and does not block completion.
    #[error(
        "resolves target '{}' is not in workspace '{}' (found in {}); auto-resolve is workspace-local — resolve it from its owning workspace with `orbit friction resolve <id>` (operator CLI) or `orbit tool run orbit.friction` with action update and status resolved (agent path), or land a covering task there",
        .0.friction_id, .0.workspace_id, .0.found_in.join(", ")
    )]
    FrictionNotLocal(Box<FrictionNotLocal>),
    /// The host's own global configuration is invalid, so a call that only
    /// needed the host to serve it was not judged. Names the host and file.
    #[error("host configuration error on host '{host}' in '{path}': {message}")]
    HostConfiguration {
        /// Name of the host whose settings prevented the call.
        host: String,
        /// Path to that host's invalid configuration file.
        path: String,
        /// Parse or validation diagnostic produced by the configuration reader.
        message: String,
    },
    /// The input cannot be accepted as submitted. The payload is the reader-facing reason.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A distributed-execution claim or handoff mutation the owner refused.
    /// Displays and reports exactly as [`Self::InvalidInput`]; `kind` is what
    /// callers branch on, never `message`.
    #[error("invalid input: {message}")]
    ClaimRefused {
        /// Stable classification of the failed ownership or handoff check.
        kind: ClaimRefusalKind,
        /// Reader-facing details supplied by the claim owner.
        message: String,
    },
    /// A refusal `--force` overrides. Displays exactly as
    /// [`Self::InvalidInput`]; the refusal's `kind` is what callers branch
    /// on, never the message.
    #[error("invalid input: {}", .0.message)]
    ForceOverridable(Box<ForceOverridableRefusal>),
    /// Input was refused to keep sensitive data out of the operation and its records.
    #[error("sensitive input rejected for `{field}`: {reason}")]
    SensitiveInput {
        /// Name of the refused input field.
        field: String,
        /// Explanation safe to report without disclosing the rejected value.
        reason: String,
    },
    /// An invalid input with plausible replacements that a caller can present separately.
    #[error("invalid input: {message}")]
    InvalidInputDiagnostic {
        /// Reader-facing reason the original input was rejected.
        message: String,
        /// Candidate replacements for the supplied value.
        did_you_mean: Vec<String>,
    },
    /// A skill definition failed structural or semantic validation.
    #[error("skill validation failed: {0}")]
    SkillValidation(String),
    /// A job definition or invocation failed validation before it could execute.
    #[error("job validation failed: {0}")]
    JobValidation(String),
    /// The provider did not satisfy the expected agent protocol.
    #[error("agent protocol violation: {0}")]
    AgentProtocolViolation(String),
    /// The requested agent provider cannot be used by this runtime.
    #[error("unsupported agent provider: {0}")]
    UnsupportedAgentProvider(String),
    /// The owner could not be reached before handing it the operation.
    #[error("owner unavailable: {0}")]
    OwnerUnavailable(String),
    /// The owner was reached, but its protocol or contract could not be negotiated.
    #[error("owner negotiation failed: {0}")]
    OwnerNegotiation(String),
    /// The operation may have reached the owner. Reconcile by its original call ID before
    /// retrying.
    #[error("owner call outcome unknown for {mcp_call_id}: {message}")]
    OutcomeUnknown {
        /// Call identity used to recover the outcome from the owner.
        mcp_call_id: String,
        /// Diagnostic explaining why no authoritative outcome was available.
        message: String,
    },
    /// The federated owner returned a structured tool refusal or error.
    #[error("remote tool failed ({code}): {message}")]
    RemoteTool {
        /// Stable classification supplied by the remote tool.
        code: RemoteToolCode,
        /// Reader-facing failure from the remote owner.
        message: String,
        /// Owner-supplied structured details available for the caller to project.
        payload: serde_json::Value,
    },
    /// A workflow or operation could not finish and has no narrower failure classification.
    #[error("execution failed: {0}")]
    Execution(String),
    /// No configured automatic crew remains after recorded model refusals.
    #[error("{0}")]
    ProviderModelAccessHold(String),
    /// A resumed published delivery's deferred footprint was still held by
    /// another task when its wait expired. Displays exactly as
    /// [`Self::Execution`]; the variant carries the typed
    /// `footprint_unavailable` step code so run finalization holds the run
    /// instead of failing it. The run-stopped refusal from the same wait
    /// stays [`Self::Execution`]: cancellation is not a hold.
    #[error("execution failed: {0}")]
    #[serde(
        rename = "Execution",
        serialize_with = "serialize_footprint_unavailable"
    )]
    FootprintUnavailable(String),
    /// Executable-generation admission refused an operation. Displays and
    /// serializes exactly as [`Self::Execution`]; callers select clock handling
    /// by the refusal kind, never from its message.
    #[error("execution failed: {}", .0.message)]
    #[serde(
        rename = "Execution",
        serialize_with = "serialize_upgrade_admission_refusal"
    )]
    UpgradeAdmissionRefused(Box<UpgradeAdmissionRefusal>),
    /// A typed fail-closed completion or landing decision. Its message remains
    /// diagnostic; the refusal value alone authorizes a held run outcome.
    #[error("execution failed: {message}")]
    DeliveryRefused {
        /// Typed decision governing whether delivery enters a hold.
        refusal: orbit_types::workflow::DeliveryRefusal,
        /// Reader guidance describing the failed delivery condition.
        message: String,
    },
    /// Conflict recovery certified that the base already carries the candidate.
    /// Keeps the execution error's display and serialized string for compatibility.
    #[error("execution failed: {0}")]
    #[serde(rename = "Execution", serialize_with = "serialize_absorbed_candidate")]
    CandidateAbsorbed(Box<CandidateAbsorbed>),
    /// Evidence for a workflow prerequisite decision could not be read or
    /// verified. Displays and serializes exactly as [`Self::Execution`]; callers
    /// select this family by the variant, never from the text.
    #[error("execution failed: {0}")]
    #[serde(
        rename = "Execution",
        serialize_with = "serialize_workflow_evidence_unavailable"
    )]
    WorkflowEvidenceUnavailable(WorkflowEvidenceUnavailable),
    /// An execution step failed because an operation it supervised hit its
    /// deadline. Reads and codes exactly as [`Self::Execution`]; the variant
    /// carries the timeout class so callers never read it from the text.
    #[error("execution failed: {message}")]
    ExecutionTimeout {
        /// Deadline of the supervised execution step, in milliseconds.
        timeout_ms: u64,
        /// Diagnostic from the step that exceeded the deadline.
        message: String,
    },
    /// A private automation VCS operation failed. Displays and serializes
    /// exactly as [`Self::Execution`]; the failure class is decided where the
    /// provider's diagnostic is read, so callers never read it from the text.
    #[error("execution failed: {}", .0.message)]
    #[serde(rename = "Execution", serialize_with = "serialize_private_vcs_failure")]
    PrivateVcs(Box<PrivateVcsFailure>),
    /// The forge refused a delivery push past its retry budget. Displays and
    /// serializes exactly as [`Self::Execution`]; the hold travels here, so
    /// callers never read it from the text.
    #[error("execution failed: {}", .0.message)]
    #[serde(rename = "Execution", serialize_with = "serialize_forge_push_hold")]
    ForgeUnavailable(Box<ForgePushHoldFailure>),
    /// A child process exceeded its deadline. The supervisor signalled the
    /// owned process group and reaped the leader; descendants that remained in
    /// that group were signalled with it.
    #[error("process timed out after {timeout_ms}ms: {detail}")]
    ProcessTimeout {
        /// Configured child-process deadline, in milliseconds.
        timeout_ms: u64,
        /// Context for which invocation timed out.
        detail: String,
    },
    /// Strict worker containment refused a launch before the worker existed.
    #[error(
        "worker containment required but unavailable: {reason}; run under a Linux systemd user manager, or drop --strict-worker-containment and set machine.worker_containment to warn"
    )]
    WorkerContainmentUnavailable {
        /// The unavailable containment prerequisite that refused the launch.
        reason: String,
    },
    /// The failed VCS operation left an unmerged index that an admitted recovery activity may
    /// repair.
    #[error(
        "recoverable VCS conflict during '{}': original base '{}', target base '{}'; {}; conflicting paths: {}",
        .0.operation,
        .0.original_base_sha,
        .0.target_base_sha,
        .0.diagnostic,
        .0.conflicting_paths.join(", ")
    )]
    RecoverableVcsConflict(Box<RecoverableVcsConflict>),
    /// Cancellation failed to confirm process teardown. The run still needs reconciliation.
    #[error(
        "run cancellation incomplete: pid={pid}, pgid={pgid:?}, term_sent={term_sent}, kill_sent={kill_sent}, leader_alive={leader_alive}, group_alive={group_alive}"
    )]
    RunCancellationIncomplete {
        /// Process ID of the recorded run leader.
        pid: u32,
        /// Owned process group, when known.
        pgid: Option<i32>,
        /// Whether the cancellation path sent the graceful termination signal.
        term_sent: bool,
        /// Whether the cancellation path sent the forced termination signal.
        kill_sent: bool,
        /// Whether the leader still appeared alive after cancellation.
        leader_alive: bool,
        /// Whether the owned process group still appeared alive after cancellation.
        group_alive: bool,
    },
    /// Cancellation could not confirm teardown of the run's open providers.
    /// No PID means the provider audit trail could not be read. Ownership must
    /// be retained until the evidence is readable and every provider is gone.
    #[error("run cancellation incomplete: provider teardown unconfirmed (pid={pid:?}): {reason}")]
    RunProviderTeardownUnconfirmed {
        /// Recorded provider PID, absent when the audit trail was unreadable.
        pid: Option<u32>,
        /// Evidence explaining why teardown could not be confirmed.
        reason: String,
    },
    /// A durable task bundle could not be read as a valid task record.
    #[error("task bundle corrupt for {task_id} at {path}: {reason}")]
    TaskBundleCorrupt {
        /// ID of the affected task record.
        task_id: String,
        /// Bundle path at which corruption was detected.
        path: String,
        /// Read or consistency failure that makes the bundle unusable.
        reason: String,
    },
    /// A bounded advisory file-lock acquisition exhausted its deadline.
    /// Structured lock and holder fields let callers diagnose contention
    /// without parsing the display message.
    #[error("{0}")]
    FileLockTimeout(Box<crate::fs::io::FileLockTimeout>),
    /// A persistence operation failed without a narrower typed storage cause.
    #[error("store error: {0}")]
    Store(String),
    /// SQLite bootstrap exhausted its busy timeout. Recovery may retry based on the structured
    /// phase.
    #[error(
        "store error: SQLite database '{}' remained locked during {}: {}",
        .0.path, .0.phase, .0.detail
    )]
    SqliteContention(Box<SqliteContention>),
    /// An update attempted to remove a protected identity tag without explicit permission.
    #[error("cannot drop system identity tag '{tag}'; pass --allow-drop-system-tags to override")]
    SystemIdentityTagDropped {
        /// Exact tag the refused update would drop.
        tag: String,
    },
    /// A workflow run was refused because a dependency that reached `done` has
    /// not been delivered into the base the run would be cut from
    /// [ORB-10464]. The lifecycle transition is legal, but the *baseline* is wrong.
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
    ShipRunInFlight {
        /// Explicit task whose second ship submission was refused.
        task_id: String,
        /// Nonterminal run already carrying the task.
        run_id: String,
    },
    /// PR delivery was refused at admission because no Git remote of the
    /// checkout names a network host, so `pr_open` could never find a forge.
    /// Raised before any run, worktree or crew exists; `remotes` describes
    /// what the checkout does have.
    #[error(
        "PR delivery needs a Git remote on a forge host, but {remotes}; nothing was dispatched. \
         Switch this workspace to local delivery with `orbit workspace ship-mode local`, or tag \
         the task `delivery:task_local_pipeline` to deliver it locally"
    )]
    PrForgeRemoteMissing {
        /// Diagnostic describing the discovered remotes and why none can host the PR.
        remotes: String,
    },
    /// An explicit dispatch named a `backlog` task whose `not_before` is still
    /// ahead. Raised by the shared workflow-entry check, so every surface
    /// refuses it identically; `not_before` is the RFC 3339 UTC time.
    #[error(
        "task '{task_id}' is scheduled: not before {not_before}; it starts on the first \
         admission pass after that time. An operator can start it now with \
         `orbit run ship {task_id}` or the dashboard's Ship"
    )]
    TaskScheduled {
        /// Explicit backlog task refused by the admission pass.
        task_id: String,
        /// Earliest eligible dispatch instant, in RFC 3339 UTC.
        not_before: String,
    },
    /// GitHub has not finished calculating whether an open pull request can
    /// merge. Callers should defer that pull request without treating the
    /// answer as a forge failure.
    #[error("pull request #{number} mergeability is unknown")]
    PullRequestMergeabilityUnknown {
        /// Number of the open PR whose mergeability is pending.
        number: u64,
    },
    /// Completion cannot overtake the linked implementation run while its
    /// recorded PID and start-time identity still name a running owner.
    #[error(
        "task '{task_id}' cannot move to done while linked run '{run_id}' has a verified-live owner"
    )]
    TaskCompletionLiveRun {
        /// Task whose completion was refused while its implementation was live.
        task_id: String,
        /// Linked run with a verified live owner.
        run_id: String,
    },
    /// A guarded desktop mutation lost its task content compare-and-set.
    #[error(
        "task '{task_id}' revision conflict; refresh authoritative state and preserve your draft"
    )]
    TaskRevisionConflict {
        /// Task whose authoritative revision changed since the caller read it.
        task_id: String,
    },
    /// The request committed, but subsequent task projection or provenance refresh failed.
    /// Adapters must report success and reconcile the original request identity.
    #[error(
        "desktop write for task '{task_id}' was accepted; refresh failed: {reason}; reconcile the same request identity"
    )]
    DesktopWriteAccepted {
        /// Task whose requested write committed.
        task_id: String,
        /// Failure that prevented the post-write state refresh.
        reason: String,
    },
    /// A resume was refused because the source run's retry lineage already
    /// has a non-terminal run. Raised atomically by the store insert on the
    /// shared resume path, so the CLI, MCP, and HTTP surfaces refuse the
    /// duplicate identically; the payload names the live run so a caller can
    /// open, wait on, or cancel it rather than resume again.
    #[error(
        "job run '{source_run_id}' already has a live resume in its retry lineage ({run_id}); wait for it to finish or cancel it before resuming again"
    )]
    ResumeRunInFlight {
        /// Run the caller asked to resume.
        source_run_id: String,
        /// Nonterminal run already carrying that retry lineage.
        run_id: String,
    },
    /// Scratch collection refuses every pending, running, or retrying workspace
    /// job, including a stale owner that has not explicitly been reconciled.
    #[error("cannot collect workspace scratch while job runs are pending, running, or retrying: {}", .run_ids.join(", "))]
    TmpGcActiveRuns {
        /// Workspace runs that must stop or be reconciled before scratch collection.
        run_ids: Vec<String>,
    },
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
    /// A requested job-run transition violated the current state machine.
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
    /// Workspace selection, configuration or checkout management could not complete.
    #[error("workspace error: {0}")]
    WorkspaceError(String),
    /// Checkout relocation cannot inspect activity or append its audit without
    /// an initialized host store. Reporting this prerequisite never creates it.
    #[error(
        "checkout rebind requires the missing host store '{}'; initialize it by running a runtime Orbit command such as `orbit tool run orbit.task.list --input '{{}}'` from a registered checkout before moving it, then retry",
        path.display()
    )]
    WorkspaceRebindHostStoreRequired {
        /// Path to the host store required before moving the checkout binding.
        path: std::path::PathBuf,
    },
    /// A filesystem or stream operation failed without a narrower typed I/O cause.
    #[error("io error: {0}")]
    Io(String),
    /// Forward schema migration could not produce a readable current record.
    #[error("schema migration failed: {0}")]
    Migration(String),
    /// A filesystem or SQLite operation failed because its target is mounted
    /// read-only or denies access. Classified from the native error (the
    /// `io::ErrorKind` or errno, or SQLite's result code) where it is
    /// translated, so passive callers that fail open never read message text.
    /// Displays as the `layer` variant, with the `storage_access_denied` wire code.
    #[error("{layer}: {message}")]
    StorageAccessDenied {
        /// Original error layer retained for display and serialization.
        layer: StorageLayer,
        /// Diagnostic from the native access failure.
        message: String,
    },
    /// A SQLite operation lost only to another connection's lock outlasting
    /// its busy timeout (`SQLITE_BUSY` or `SQLITE_LOCKED`). Classified from
    /// SQLite's result code where the error is translated, so callers answer
    /// it with [`Self::is_lock_busy`] instead of reading message text.
    /// Displays, reports and serializes exactly as the `layer` variant it
    /// stands in for.
    #[error("{layer}: {message}")]
    #[serde(untagged, serialize_with = "serialize_as_storage_layer")]
    StorageLockBusy {
        /// Layer whose display and wire identity the lock failure retains.
        layer: StorageLayer,
        /// Diagnostic produced by the blocked storage operation.
        message: String,
    },
    /// A named source-repair authority refusal. Displays, reports and
    /// serializes exactly as the [`Self::JobValidation`] (or, for
    /// [`SourceRepairAuthorityRefusal::PublicationAdmissionChanged`],
    /// [`Self::Execution`]) error it stands in for; the code is attached where
    /// the refusal is decided, so callers never read it from the text.
    #[error("{}", source_repair_refusal_legacy_error(*.0))]
    #[serde(untagged, serialize_with = "serialize_source_repair_refusal")]
    SourceRepairAuthorityRefused(SourceRepairAuthorityRefusal),
    /// A source-repair refusal caused by the run's state. Displays, reports
    /// and serializes exactly as the [`Self::JobValidation`] carrying
    /// `message`; the `reason` is attached where the refusal is decided, so
    /// callers never read it from the text.
    #[error("job validation failed: {message}")]
    #[serde(untagged, serialize_with = "serialize_source_repair_run_state_refusal")]
    SourceRepairRunStateRefused {
        /// Run-state classification attached by the source-repair admission check.
        reason: SourceRepairRunStateRefusal,
        /// Reader-facing details of the state that refused the repair.
        message: String,
    },
    /// A source-repair refusal that waits on an operator decision (discard
    /// the candidate or select an explicit source). Displays, reports and
    /// serializes exactly as the [`Self::JobValidation`] carrying `message`;
    /// the variant is attached where the refusal is decided, so callers
    /// never classify it from the text.
    #[error("job validation failed: {message}")]
    #[serde(untagged, serialize_with = "serialize_source_repair_operator_decision")]
    SourceRepairOperatorDecisionRefused {
        /// Reader guidance identifying the unresolved source-repair choice.
        message: String,
    },
    /// No delivery run submitted this task in the selected workspace.
    /// Retains the invalid-input wire contract while surfaces render their
    /// own task-read remedy from the task ID.
    #[error("invalid input: {}", task_delivery_unavailable_message(.0, &format!("orbit task show {}", .0)))]
    #[serde(untagged, serialize_with = "serialize_task_delivery_unavailable")]
    TaskDeliveryUnavailable(String),
    /// An observation-only open found a supported store or task registry that
    /// still owes a writable open (for example migration or additive setup),
    /// rather than an unreadable or unsupported format. Displays, reports and
    /// serializes identically to [`Self::Store`]; callers select writable-open
    /// remediation from this variant alone, never from message text (STD-02 §R10).
    #[error("store error: {0}")]
    #[serde(untagged, serialize_with = "serialize_as_store")]
    StoreWritableOpenOwed(String),
    /// A workspace selector matched neither a registered logical workspace nor
    /// a local checkout path. Displays, reports and serializes identically to
    /// [`Self::InvalidInput`]; callers branch on this variant alone, never on
    /// diagnostic prose (STD-02 §R10).
    #[error("invalid input: {}", .0.message())]
    #[serde(untagged, serialize_with = "serialize_unknown_workspace_selector")]
    UnknownWorkspaceSelector(Box<UnknownWorkspaceSelector>),
}

const _: () = assert!(
    std::mem::size_of::<OrbitError>() <= 128,
    "OrbitError exceeds its 128-byte size budget; box multi-field variant payloads"
);

impl OrbitError {
    /// Render task/run remedies using the advertised MCP tool names and JSON
    /// arguments. Explicit not-found guidance still takes precedence.
    pub fn mcp_error_message(&self) -> String {
        match self {
            Self::NotFound {
                kind: NotFoundKind::Task,
                id,
                guidance: None,
            } => not_found_message(
                NotFoundKind::Task,
                id,
                Some("; see `orbit_task_list {}` for valid task IDs"),
            ),
            Self::NotFound {
                kind: NotFoundKind::JobRun,
                id,
                guidance: None,
            } => {
                let default_guidance;
                let guidance = if orbit_types::task::is_valid_orb_task_id(id) {
                    let input = serde_json::json!({"id": id, "field": "delivery"});
                    default_guidance = format!(
                        "; see `orbit_run {{\"action\":\"list\"}}` for valid run IDs, or use `orbit_task_show {input}` for a task's delivery run"
                    );
                    &default_guidance
                } else {
                    "; see `orbit_run {\"action\":\"list\"}` for valid run IDs"
                };
                not_found_message(NotFoundKind::JobRun, id, Some(guidance))
            }
            Self::TaskDeliveryUnavailable(task_id) => {
                let input = serde_json::json!({"id": task_id});
                format!(
                    "invalid input: {}",
                    task_delivery_unavailable_message(task_id, &format!("orbit_task_show {input}"))
                )
            }
            _ => self.to_string(),
        }
    }
}

fn not_found_message(kind: NotFoundKind, id: &str, guidance: Option<&str>) -> String {
    // This synthetic partition represents an initialized data root without a
    // registered workspace. It is an internal routing key, so show the setup
    // action instead of presenting that key as a workspace the operator can
    // find.
    if kind == NotFoundKind::Workspace && id == "ws_unbound-data-dir" {
        return "workspace not found; run `orbit workspace init` from a project or pass `--workspace <workspace>`".to_string();
    }

    // Explicit guidance (for example a spelling suggestion or an artifact's
    // owning task) is more specific than the default listing.
    let default_guidance;
    let guidance = guidance.unwrap_or(match kind {
        NotFoundKind::Task => "; see `orbit task list` for valid task IDs",
        NotFoundKind::JobRun => {
            if orbit_types::task::is_valid_orb_task_id(id) {
                default_guidance = format!("; see `orbit run history` for valid run IDs, or use `orbit task show {id} --fields delivery` for a task's delivery run");
                &default_guidance
            } else {
                "; see `orbit run history` for valid run IDs"
            }
        }
        NotFoundKind::Job => "; see `orbit job list` for valid job IDs",
        NotFoundKind::Tool => "; see `orbit tool list` or MCP `tools/list` for valid tool names",
        NotFoundKind::Workspace => "; see `orbit workspace list` for registered workspaces",
        _ => "",
    });
    format!("{kind} not found: {id}{guidance}")
}

fn task_delivery_unavailable_message(task_id: &str, remedy: &str) -> String {
    format!(
        "no task delivery run in this workspace was submitted with task '{task_id}'; delivery evidence exists only for tasks submitted by a delivery run in this workspace — use `{remedy}` instead"
    )
}

fn serialize_task_delivery_unavailable<S: serde::Serializer>(
    task_id: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    OrbitError::InvalidInput(task_delivery_unavailable_message(
        task_id,
        &format!("orbit task show {task_id}"),
    ))
    .serialize(serializer)
}

fn serialize_unknown_workspace_selector<S: serde::Serializer>(
    unknown: &UnknownWorkspaceSelector,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    OrbitError::InvalidInput(unknown.message()).serialize(serializer)
}

fn serialize_footprint_unavailable<S: serde::Serializer>(
    message: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(message)
}

fn serialize_absorbed_candidate<S: serde::Serializer>(
    candidate: &CandidateAbsorbed,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&candidate.to_string())
}

fn serialize_upgrade_admission_refusal<S: serde::Serializer>(
    refusal: &UpgradeAdmissionRefusal,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&refusal.message)
}

fn serialize_workflow_evidence_unavailable<S: serde::Serializer>(
    evidence: &WorkflowEvidenceUnavailable,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&evidence.to_string())
}

fn serialize_private_vcs_failure<S: serde::Serializer>(
    failure: &PrivateVcsFailure,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&failure.message)
}

fn serialize_forge_push_hold<S: serde::Serializer>(
    failure: &ForgePushHoldFailure,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&failure.message)
}

fn serialize_as_store<S: serde::Serializer>(
    message: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    OrbitError::Store(message.to_string()).serialize(serializer)
}

fn serialize_as_storage_layer<S: serde::Serializer>(
    layer: &StorageLayer,
    message: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    OrbitError::storage(*layer, false, message).serialize(serializer)
}

/// The untyped error a source-repair authority refusal reports as.
fn source_repair_refusal_legacy_error(code: SourceRepairAuthorityRefusal) -> OrbitError {
    if code.reports_as_execution() {
        OrbitError::Execution(code.message())
    } else {
        OrbitError::JobValidation(code.message())
    }
}

fn serialize_source_repair_run_state_refusal<S: serde::Serializer>(
    _reason: &SourceRepairRunStateRefusal,
    message: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    OrbitError::JobValidation(message.to_string()).serialize(serializer)
}

fn serialize_source_repair_operator_decision<S: serde::Serializer>(
    message: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    OrbitError::JobValidation(message.to_string()).serialize(serializer)
}

fn serialize_source_repair_refusal<S: serde::Serializer>(
    code: &SourceRepairAuthorityRefusal,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    source_repair_refusal_legacy_error(*code).serialize(serializer)
}
