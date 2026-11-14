use std::path::Path;

use super::{
    ArtifactOrigin, ClaimRefusalKind, DecisionRefusalCode, ForceOverridableKind,
    ForceOverridableRefusal, ForgePushHoldFailure, FrictionNotLocal, HostRegistryCode,
    NotFoundKind, NoticeRefusalCode, OrbitError, PrivateVcsFailure, RemoteToolCode,
    SourceRepairAuthorityRefusal, SourceRepairRunStateRefusal, SqliteContention, StorageLayer,
    TaskPilotRefusalCode, WorkflowEvidenceUnavailable, WorkspaceClaimHeld,
};

/// The JSON-RPC invalid-params code. An owner whose internal drain
/// registry predates an operation answers it with this code.
pub const JSON_RPC_INVALID_PARAMS_CODE: i64 = -32602;

/// The structured `message` field of the legacy invalid-params reply for an
/// unknown internal drain operation. Compared as a whole structured field,
/// never searched inside a formatted message.
const UNKNOWN_INTERNAL_DRAIN_OPERATION_RPC_MESSAGE: &str = "unknown internal drain operation";

impl OrbitError {
    /// Stable machine-readable code shared by Orbit error surfaces.
    pub fn wire_code(&self) -> &str {
        match self {
            OrbitError::NoJobRuns => const { RemoteToolCode::NotFound.as_str() },
            OrbitError::NotFound { kind, .. } => match kind {
                NotFoundKind::Tool => "tool_not_found",
                NotFoundKind::Task
                | NotFoundKind::Friction
                | NotFoundKind::Artifact
                | NotFoundKind::Skill
                | NotFoundKind::Job
                | NotFoundKind::JobRun
                | NotFoundKind::Workspace => const { RemoteToolCode::NotFound.as_str() },
            },
            OrbitError::PolicyDenied(_) => const { RemoteToolCode::PolicyDenied.as_str() },
            OrbitError::CapabilityDenied(_) => const { RemoteToolCode::CapabilityDenied.as_str() },
            OrbitError::UnknownSelector(_) => "unknown_selector",
            OrbitError::AmbiguousDestination(_) => "ambiguous_destination",
            OrbitError::UnreachableDestination(_) => "unreachable_destination",
            OrbitError::StaleRoute(_) => "stale_route",
            OrbitError::UnhealthyCheckout(_) => "unhealthy_checkout",
            OrbitError::ToolNotOnThisHost(_) => "tool_not_on_this_host",
            OrbitError::CapabilityRefused(_) => {
                const { RemoteToolCode::CapabilityRefused.as_str() }
            }
            OrbitError::ProtocolSkew(_) => const { RemoteToolCode::ProtocolSkew.as_str() },
            OrbitError::DecisionRefused { code, .. } => code.as_str(),
            OrbitError::NoticeRefused { code, .. } => code.as_str(),
            OrbitError::TaskPilotRefused { code, .. } => code.as_str(),
            OrbitError::HostRegistry { code, .. } => code.as_str(),
            OrbitError::PluginDisabledInWorkspace { .. } => "plugin_disabled_in_workspace",
            OrbitError::PluginDisabledOnHost { .. } => "plugin_disabled_on_host",
            OrbitError::PluginBuildConsentRequired(_) => "build_consent_required",
            OrbitError::PluginBuildConsentUnavailable(_) => "build_consent_unavailable",
            OrbitError::PluginBuildFetchUnsupported(_) => "build_fetch_unsupported_on_macos",
            OrbitError::HostConfiguration { .. } => {
                const { RemoteToolCode::HostConfiguration.as_str() }
            }
            OrbitError::ForceOverridable(_) => "force_overridable",
            OrbitError::InvalidInput(_)
            | OrbitError::InvalidInputDiagnostic { .. }
            | OrbitError::ClaimRefused { .. } => const { RemoteToolCode::InvalidInput.as_str() },
            OrbitError::TaskCompletionLiveRun { .. } => "task_completion_live_run",
            OrbitError::SensitiveInput { .. } => "sensitive_input",
            OrbitError::SkillValidation(_) | OrbitError::JobValidation(_) => "validation_failed",
            OrbitError::SourceRepairRunStateRefused { .. } => "validation_failed",
            OrbitError::SourceRepairAuthorityRefused(code) => {
                if code.reports_as_execution() {
                    "execution_failed"
                } else {
                    "validation_failed"
                }
            }
            OrbitError::JobRunStateTransition(_) => "invalid_transition",
            // [ORB-10965] Not "invalid_transition": a duplicate start losing to the
            // incumbent owner is an expected race, and the caller yields rather
            // than treating its own request as malformed.
            OrbitError::JobRunStartConflict(_) => "job_run_start_conflict",
            // [ORB-11253, ORB-12260] A compare-and-set revision conflict on a
            // run-control update or operation grant transition.
            OrbitError::JobRunControlConflict(_) => "conflict",
            OrbitError::DependencyNotDelivered { .. } => "dependency_not_delivered",
            OrbitError::ShipRunInFlight { .. } => "ship_run_in_flight",
            OrbitError::PrForgeRemoteMissing { .. } => "pr_forge_remote_missing",
            OrbitError::TaskScheduled { .. } => "task_scheduled",
            OrbitError::ResumeRunInFlight { .. } => "resume_run_in_flight",
            OrbitError::WorkspaceClaimHeld(_) => "workspace_claim_held",
            OrbitError::RemoteArtifactUnavailable { .. } => "remote_artifact_unavailable",
            OrbitError::ArtifactNotLocal { .. } => "artifact_not_local",
            OrbitError::FrictionNotLocal(_) => "friction_not_local",
            OrbitError::AgentProtocolViolation(_) => "agent_protocol_violation",
            OrbitError::UnsupportedAgentProvider(_) => "unsupported_provider",
            OrbitError::OwnerUnavailable(_) => "owner_unavailable",
            OrbitError::OwnerNegotiation(_) => "owner_negotiation",
            OrbitError::OutcomeUnknown { .. } => "outcome_unknown",
            OrbitError::RemoteTool { code, .. } => code.as_str(),
            OrbitError::DeliveryRefused { refusal, .. } => refusal.error_code(),
            OrbitError::Execution(_)
            | OrbitError::PrivateVcs(_)
            | OrbitError::ForgeUnavailable(_)
            | OrbitError::WorkflowEvidenceUnavailable(_)
            | OrbitError::ExecutionTimeout { .. } => "execution_failed",
            OrbitError::ProcessTimeout { .. } => "process_timeout",
            OrbitError::TaskBundleCorrupt { .. } => "task_bundle_corrupt",
            OrbitError::Store(_) => "store_error",
            // [ORB-15088] Retryable, unlike `internal_error`: a pull follower
            // retries an owner read that lost only to a lock-wait deadline and
            // never mistakes it for a failure of its candidate.
            OrbitError::FileLockTimeout(_) | OrbitError::SqliteContention(_) => {
                LOCK_BUSY_ERROR_CODE
            }
            OrbitError::WorkspaceError(_) => "workspace_error",
            OrbitError::Io(_) => "io_error",
            OrbitError::Migration(_) => "migration_failed",
            OrbitError::StorageAccessDenied { .. } => "storage_access_denied",
            OrbitError::StorageLockBusy { layer, .. } => layer.error_code(),
            OrbitError::CallbackWorkspaceOutsideSandbox(_) => "callback_workspace_outside_sandbox",
            OrbitError::WorkerContainmentUnavailable { .. } => "worker_containment_unavailable",
            OrbitError::TmpGcActiveRuns { .. } => "tmp_gc_active_runs",
            OrbitError::SystemIdentityTagDropped { .. } => "system_identity_tag_dropped",
            OrbitError::CandidateAbsorbed(_)
            | OrbitError::DesktopWriteAccepted { .. }
            | OrbitError::PullRequestMergeabilityUnknown { .. }
            | OrbitError::RecoverableVcsConflict(_)
            | OrbitError::RunCancellationIncomplete { .. }
            | OrbitError::RunProviderTeardownUnconfirmed { .. }
            | OrbitError::TaskRevisionConflict { .. } => "internal_error",
        }
    }

    /// Structured error details shared by Orbit error surfaces.
    pub fn wire_details(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut fields = serde_json::Map::new();
        if let Some((host, path, _)) = self.host_configuration_details() {
            fields.insert("host".to_string(), serde_json::json!(host));
            fields.insert("path".to_string(), serde_json::json!(path));
        }
        if let Some(did_you_mean) = self.did_you_mean() {
            fields.insert("did_you_mean".to_string(), serde_json::json!(did_you_mean));
        }
        if let Some(artifact_origin) = self.artifact_origin() {
            fields.insert(
                "artifact_origin".to_string(),
                serde_json::json!(artifact_origin),
            );
        }
        if let Some(refusal) = self.force_overridable() {
            fields.insert(
                "guard".to_string(),
                serde_json::json!(refusal.kind.as_str()),
            );
            if let Some(run_id) = &refusal.run_id {
                fields.insert("run_id".to_string(), serde_json::json!(run_id));
            }
            if let Some(run_state) = &refusal.run_state {
                fields.insert("run_state".to_string(), serde_json::json!(run_state));
            }
            if let Some(task_status) = &refusal.task_status {
                fields.insert("task_status".to_string(), serde_json::json!(task_status));
            }
        }
        if let Some((task_id, run_id)) = self.ship_run_in_flight() {
            fields.insert("task_id".to_string(), serde_json::json!(task_id));
            fields.insert("run_id".to_string(), serde_json::json!(run_id));
        }
        if let OrbitError::TaskCompletionLiveRun { task_id, run_id } = self {
            fields.insert("task_id".to_string(), serde_json::json!(task_id));
            fields.insert("run_id".to_string(), serde_json::json!(run_id));
        }
        if let Some((source_run_id, run_id)) = self.resume_run_in_flight() {
            fields.insert(
                "source_run_id".to_string(),
                serde_json::json!(source_run_id),
            );
            fields.insert("run_id".to_string(), serde_json::json!(run_id));
        }
        if let Some((task_id, path, reason)) = self.task_bundle_corruption() {
            fields.insert("task_id".to_string(), serde_json::json!(task_id));
            fields.insert("path".to_string(), serde_json::json!(path));
            fields.insert("reason".to_string(), serde_json::json!(reason));
        }
        if let Some(details) = self.friction_not_local_details() {
            fields.insert(
                "friction_id".to_string(),
                serde_json::json!(details.friction_id),
            );
            fields.insert("task_id".to_string(), serde_json::json!(details.task_id));
            fields.insert(
                "workspace_id".to_string(),
                serde_json::json!(details.workspace_id),
            );
            fields.insert("found_in".to_string(), serde_json::json!(details.found_in));
        }
        if let OrbitError::TmpGcActiveRuns { run_ids } = self {
            fields.insert("run_ids".to_string(), serde_json::json!(run_ids));
        }
        if let Some(tag) = self.dropped_system_tag() {
            fields.insert("tag".to_string(), serde_json::json!(tag));
        }
        fields
    }

    /// Preserve a destination's JSON-RPC error answer as structured data.
    ///
    /// The numeric `code` and string `message` fields are kept typed in the
    /// [`Self::RemoteTool`] payload (`rpc_code` / `rpc_message`), so callers
    /// classify from those fields rather than a formatted string. The
    /// formatted text stays only as the human-readable detail.
    pub fn json_rpc_refusal(machine_id: &str, method: &str, error: &serde_json::Value) -> Self {
        let rpc_code = error.get("code").and_then(serde_json::Value::as_i64);
        let rpc_message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let code = match rpc_code {
            Some(JSON_RPC_INVALID_PARAMS_CODE) => RemoteToolCode::InvalidInput,
            Some(-32601) => RemoteToolCode::NotFound,
            Some(other) => RemoteToolCode::Other(format!("json_rpc_{other}")),
            None => RemoteToolCode::Other("json_rpc_error".to_string()),
        };
        let mut payload = serde_json::json!({
            "code": code.as_str(),
            "message": rpc_message,
            "rpc_message": rpc_message,
        });
        if let Some(rpc_code) = rpc_code {
            payload["rpc_code"] = serde_json::json!(rpc_code);
        }
        if let Some(data) = error.get("data") {
            payload["rpc_data"] = data.clone();
        }
        Self::RemoteTool {
            code,
            message: format!("{machine_id}: '{method}' failed: {error}"),
            payload,
        }
    }

    /// Whether this error is a preserved JSON-RPC refusal.
    pub fn is_json_rpc_refusal(&self) -> bool {
        matches!(self, Self::RemoteTool { payload, .. } if payload.get("rpc_message").is_some())
    }

    /// Whether this is the legacy owner's unknown-internal-drain-operation
    /// refusal, decided from its structured RPC code and message fields.
    pub fn is_unknown_internal_drain_operation(&self) -> bool {
        match self {
            Self::RemoteTool { payload, .. } => {
                payload.get("rpc_code").and_then(serde_json::Value::as_i64)
                    == Some(JSON_RPC_INVALID_PARAMS_CODE)
                    && payload
                        .get("rpc_message")
                        .and_then(serde_json::Value::as_str)
                        == Some(UNKNOWN_INTERNAL_DRAIN_OPERATION_RPC_MESSAGE)
            }
            _ => false,
        }
    }

    /// The typed step-failure code this error carries, parsed once where a
    /// step failure is ingested from an `OrbitError`.
    ///
    /// An ordinary remote-tool failure contributes its wire code verbatim. The
    /// owner-route variants — a destination that cannot be reached, an
    /// owner that is unavailable or cannot be negotiated with, a stale
    /// route to it — are the [`StepFailureCode::OwnerRouteUnavailable`]
    /// the claimed worker's owner route raises; their messages are kept
    /// as they are, and the typing happens here instead of in a bracketed
    /// marker stamped into the text. A preserved JSON-RPC refusal also
    /// classifies as [`StepFailureCode::OwnerRouteUnavailable`], keeping
    /// the typing it had before PR #9155 preserved it as a remote-tool reply.
    ///
    /// [`StepFailureCode::OwnerRouteUnavailable`]: orbit_types::workflow::StepFailureCode::OwnerRouteUnavailable
    pub fn step_failure_code(&self) -> Option<orbit_types::workflow::StepFailureCode> {
        if self.is_json_rpc_refusal() {
            return Some(orbit_types::workflow::StepFailureCode::OwnerRouteUnavailable);
        }
        match self {
            Self::RemoteTool { code, .. } => {
                Some(orbit_types::workflow::StepFailureCode::parse(code.as_str()))
            }
            Self::UnreachableDestination(_)
            | Self::OwnerUnavailable(_)
            | Self::StaleRoute(_)
            | Self::OwnerNegotiation(_) => {
                Some(orbit_types::workflow::StepFailureCode::OwnerRouteUnavailable)
            }
            _ => None,
        }
    }

    /// Construct a decision refusal without encoding its classification in prose.
    pub fn decision_refused(code: DecisionRefusalCode, message: impl Into<String>) -> Self {
        Self::DecisionRefused {
            code,
            message: message.into(),
        }
    }

    /// Construct a fleet-notice refusal with its stable code.
    pub fn notice_refused(code: NoticeRefusalCode, message: impl Into<String>) -> Self {
        Self::NoticeRefused {
            code,
            message: message.into(),
        }
    }

    /// Construct an explicit task-pilot submission refusal with its stable code.
    pub fn task_pilot_refused(code: TaskPilotRefusalCode, message: impl Into<String>) -> Self {
        Self::TaskPilotRefused {
            code,
            message: message.into(),
        }
    }

    /// Construct the unavailable-evidence failure of a workflow prerequisite read.
    pub fn workflow_evidence_unavailable(reason: impl Into<String>) -> Self {
        Self::WorkflowEvidenceUnavailable(WorkflowEvidenceUnavailable {
            reason: reason.into(),
        })
    }

    /// Whether this is a policy denial, including a typed decision-channel refusal.
    pub fn is_policy_denial(&self) -> bool {
        match self {
            Self::PolicyDenied(_) => true,
            Self::DecisionRefused { code, .. } => code.is_policy_denial(),
            Self::NoticeRefused { code, .. } => code.is_policy_denial(),
            _ => false,
        }
    }

    pub fn host_registry(code: HostRegistryCode, message: impl Into<String>) -> Self {
        Self::HostRegistry {
            code,
            message: message.into(),
        }
    }

    /// The host-registry code of this error, if it is one.
    pub fn host_registry_code(&self) -> Option<HostRegistryCode> {
        match self {
            Self::HostRegistry { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn not_found(kind: NotFoundKind, id: impl Into<String>) -> Self {
        Self::NotFound {
            kind,
            id: id.into(),
            guidance: None,
        }
    }

    /// Construct a typed not-found error with a next step for the caller.
    pub fn not_found_with_guidance(
        kind: NotFoundKind,
        id: impl Into<String>,
        guidance: impl Into<String>,
    ) -> Self {
        Self::NotFound {
            kind,
            id: id.into(),
            guidance: Some(guidance.into()),
        }
    }

    /// Construct a typed host-configuration error naming the host and file.
    pub fn host_configuration(
        host: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::HostConfiguration {
            host: host.into(),
            path: path.into(),
            message: message.into(),
        }
    }

    /// The `(host, path, message)` of a host-configuration error, if it is one.
    pub fn host_configuration_details(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::HostConfiguration {
                host,
                path,
                message,
            } => Some((host, path, message)),
            _ => None,
        }
    }

    /// Whether this is a typed host-configuration error, including one a
    /// remote owner reported as `host_configuration`.
    pub fn is_host_configuration(&self) -> bool {
        matches!(
            self,
            Self::HostConfiguration { .. }
                | Self::RemoteTool {
                    code: RemoteToolCode::HostConfiguration,
                    ..
                }
        )
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

    pub fn dropped_system_tag(&self) -> Option<&str> {
        match self {
            Self::SystemIdentityTagDropped { tag } => Some(tag),
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

    /// The `(source_run_id, run_id)` of a duplicate-resume refusal: the run the
    /// caller asked to resume and the live run already carrying its lineage.
    pub fn resume_run_in_flight(&self) -> Option<(&str, &str)> {
        match self {
            Self::ResumeRunInFlight {
                source_run_id,
                run_id,
            } => Some((source_run_id, run_id)),
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

    pub fn sqlite_contention(&self) -> Option<&SqliteContention> {
        match self {
            Self::SqliteContention(contention) => Some(contention),
            _ => None,
        }
    }

    /// The classified private automation VCS failure, if this is one.
    pub fn private_vcs_failure(&self) -> Option<&PrivateVcsFailure> {
        match self {
            Self::PrivateVcs(failure) => Some(failure),
            _ => None,
        }
    }

    /// The delivery push the forge held, if this is one.
    pub fn forge_push_hold(&self) -> Option<&ForgePushHoldFailure> {
        match self {
            Self::ForgeUnavailable(failure) => Some(failure),
            _ => None,
        }
    }

    /// The named source-repair authority refusal, if this is one.
    pub fn source_repair_authority_refusal(&self) -> Option<SourceRepairAuthorityRefusal> {
        match self {
            Self::SourceRepairAuthorityRefused(code) => Some(*code),
            _ => None,
        }
    }

    /// The run-state reason of a source-repair refusal, if this is one.
    pub fn source_repair_run_state_refusal(&self) -> Option<SourceRepairRunStateRefusal> {
        match self {
            Self::SourceRepairRunStateRefused { reason, .. } => Some(*reason),
            _ => None,
        }
    }

    /// The message of a job-validation refusal: a [`Self::JobValidation`],
    /// or a typed source-repair refusal that reports as one. Callers that
    /// answer job-validation refusals specially use this so the typed
    /// refusal keeps the treatment its untyped predecessor had.
    pub fn job_validation_message(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::JobValidation(message) => Some(message.as_str().into()),
            Self::SourceRepairAuthorityRefused(code) if !code.reports_as_execution() => {
                Some(code.message().into())
            }
            Self::SourceRepairRunStateRefused { message, .. } => Some(message.as_str().into()),
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
        match crate::fs::io::write_access_error_message(path, &err) {
            Some(message) => Self::StorageAccessDenied {
                layer: StorageLayer::Io,
                message,
            },
            None => Self::Io(err.to_string()),
        }
    }

    /// An I/O failure described by `message`, keeping whether `err` was a
    /// read-only or access denial. Use it wherever an `io::Error` is wrapped
    /// with context instead of converted with `?`.
    pub fn io_with_context(err: &std::io::Error, message: impl Into<String>) -> Self {
        Self::storage_io(StorageLayer::Io, err, message)
    }

    /// A `layer` failure described by `message` and caused by `err`, keeping
    /// whether `err` was a read-only or access denial.
    pub fn storage_io(
        layer: StorageLayer,
        err: &std::io::Error,
        message: impl Into<String>,
    ) -> Self {
        Self::storage(
            layer,
            crate::fs::io::is_readonly_or_access_error(err),
            message,
        )
    }

    /// A `layer` failure described by `message`, carried as
    /// [`Self::StorageAccessDenied`] when the native error that caused it was
    /// a read-only or access denial.
    pub fn storage(layer: StorageLayer, access_denied: bool, message: impl Into<String>) -> Self {
        let message = message.into();
        if access_denied {
            return Self::StorageAccessDenied { layer, message };
        }
        match layer {
            StorageLayer::Io => Self::Io(message),
            StorageLayer::Store => Self::Store(message),
            StorageLayer::Migration => Self::Migration(message),
        }
    }

    /// The storage family this error reports as — [`Self::Io`],
    /// [`Self::Store`] or [`Self::Migration`], whether or not it carries a
    /// read-only, access or lock-busy class — or `None` for any other error.
    pub fn storage_layer(&self) -> Option<StorageLayer> {
        match self {
            Self::Io(_) => Some(StorageLayer::Io),
            Self::Store(_) => Some(StorageLayer::Store),
            Self::Migration(_) => Some(StorageLayer::Migration),
            Self::StorageAccessDenied { layer, .. } | Self::StorageLockBusy { layer, .. } => {
                Some(*layer)
            }
            _ => None,
        }
    }

    /// Whether an operation failed because its persistence target is mounted
    /// read-only or denies writes.
    ///
    /// Answered from the variant alone: the class is decided where the native
    /// I/O or SQLite error is translated, so an error that merely quotes such
    /// text (a child's stderr, a remote message) never counts.
    pub fn is_readonly_or_access_failure(&self) -> bool {
        matches!(self, Self::StorageAccessDenied { .. })
    }

    /// A claim refusal reported with the store's text for `kind`.
    pub fn claim_refused(kind: ClaimRefusalKind) -> Self {
        Self::ClaimRefused {
            kind,
            message: kind.message().to_string(),
        }
    }

    /// A refusal `--force` overrides: a run still owns the task.
    pub fn force_overridable_run_owned(
        run_id: impl Into<String>,
        run_state: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::ForceOverridable(Box::new(ForceOverridableRefusal {
            kind: ForceOverridableKind::RunStillOwnsTask,
            message: message.into(),
            run_id: Some(run_id.into()),
            run_state: Some(run_state.into()),
            task_status: None,
        }))
    }

    /// A refusal `--force` overrides: delete outside `proposed`/`rejected`.
    pub fn force_overridable_delete(
        task_status: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::ForceOverridable(Box::new(ForceOverridableRefusal {
            kind: ForceOverridableKind::DeleteOutsideProposedOrRejected,
            message: message.into(),
            run_id: None,
            run_state: None,
            task_status: Some(task_status.into()),
        }))
    }

    /// The force-overridable refusal, if this is one.
    pub fn force_overridable(&self) -> Option<&ForceOverridableRefusal> {
        match self {
            Self::ForceOverridable(refusal) => Some(refusal),
            _ => None,
        }
    }

    /// The kind of an owner claim or handoff refusal, if this is one.
    pub fn claim_refusal(&self) -> Option<ClaimRefusalKind> {
        match self {
            Self::ClaimRefused { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Whether this failed only because a lock stayed held past its
    /// acquisition deadline: an advisory file lock or a SQLite database here,
    /// or either on a remote owner, which reports it as
    /// [`RemoteToolCode::LockBusy`]. Nothing was decided under the lock, so
    /// the same call can succeed once its holder lets go.
    ///
    /// Answered from the variant alone, like
    /// [`Self::is_readonly_or_access_failure`]: a SQLite busy or locked
    /// result is classified where the `rusqlite::Error` is translated.
    pub fn is_lock_busy(&self) -> bool {
        matches!(
            self,
            Self::FileLockTimeout(_)
                | Self::SqliteContention(_)
                | Self::StorageLockBusy { .. }
                | Self::RemoteTool {
                    code: RemoteToolCode::LockBusy,
                    ..
                }
        )
    }

    /// Whether a supervised operation exceeded its deadline: a process
    /// timeout, or an execution failure raised for one.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::ProcessTimeout { .. } | Self::ExecutionTimeout { .. }
        )
    }
}

/// The stable tool error code for a call that lost only to a lock-wait
/// deadline ([`OrbitError::is_lock_busy`]). Retryable: unlike
/// `internal_error`, it says the call can succeed unchanged later.
pub const LOCK_BUSY_ERROR_CODE: &str = RemoteToolCode::LockBusy.as_str();

/// Whether an error's text says it lost only to a lock-wait deadline, for an
/// error that reached this process as text rather than as a typed
/// [`OrbitError`]: SQLite's own `database is locked` / `database is busy` /
/// `database table is locked`, which a store error quotes; the text of a
/// `SQLITE_BUSY` that carried no message (`The database file is locked`);
/// the `code=DatabaseBusy` / `code=DatabaseLocked` markers a store error's
/// detail carries; or the display of [`OrbitError::SqliteContention`] or
/// [`OrbitError::FileLockTimeout`].
///
/// The one place such text is recognised, so the patterns change together
/// with the displays they match.
pub fn is_lock_busy_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database is busy")
        || message.contains("database table is locked")
        || message.contains("the database file is locked")
        || message.contains("code=databasebusy")
        || message.contains("code=databaselocked")
        // `OrbitError::SqliteContention`.
        || message.contains("remained locked during")
        // `FileLockTimeout`: "timed out after {ms}ms acquiring {label} lock '{path}'".
        || (message.starts_with("timed out after ")
            && message.contains("ms acquiring ")
            && message.contains(" lock '"))
}
