//! Host-local application authority: exact calls, never a capability grant.

use std::cell::RefCell;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, agent_context_declared,
};
use orbit_common::security::application_credential::ApplicationCredential;
use orbit_store::contracts::{ApplicationConsentRecord, ApplicationConsentStoreBackend};
use orbit_types::tool::{
    ApplicationConsent, ApplicationConsentRequest, ApplicationInvocation, ApplicationOperation,
    McpCapability, ToolSessionContext,
};
use serde_json::Value;

use crate::{RuntimeKernel, tool_exec::CapabilityEnforcement};

thread_local! {
    static APPLICATION_AUDIT: RefCell<Option<ApplicationInvocation>> = const { RefCell::new(None) };
}

/// Drain the host-authenticated facts for the enclosing dispatch's audit row.
pub fn take_application_audit() -> Option<ApplicationInvocation> {
    APPLICATION_AUDIT.with(|state| state.borrow_mut().take())
}

/// Calls supported by the built-in application integration. All other built-in
/// operations retain their ordinary authority requirements, even in a consent.
fn application_builtin_supported(tool: &str) -> bool {
    orbit_common::governance::authorization::APPLICATION_BUILTIN_OPERATIONS.contains(&tool)
}

impl RuntimeKernel {
    /// File-backed CLI adapter. A provisioning failure revokes the unusable
    /// mint; no credential is printed, even when an output path already exists.
    pub fn provision_application_from_file(
        &self,
        input: &Path,
        output: &Path,
        replaces: Option<&str>,
    ) -> Result<ApplicationConsent, OrbitError> {
        use std::io::Read;
        let mut raw = Vec::new();
        std::fs::File::open(input)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut raw)?;
        if raw.len() > 1024 * 1024 {
            return Err(OrbitError::InvalidInput(
                "application consent request exceeds 1 MiB".into(),
            ));
        }
        let request = serde_json::from_slice(&raw).map_err(|_| {
            OrbitError::InvalidInput("application consent request is not valid JSON".into())
        })?;
        let (consent, credential) =
            self.consent_application(request, &ToolSessionContext::default(), replaces)?;
        if let Err(error) = credential.provision(output) {
            self.revoke_application(&consent.consent_id, &ToolSessionContext::default())?;
            return Err(error);
        }
        Ok(consent)
    }
    /// Operator-only, one-time mint result. Provision it with
    /// `ApplicationCredential::provision`, never serialize or log it.
    pub fn consent_application(
        &self,
        request: ApplicationConsentRequest,
        session: &ToolSessionContext,
        replaces: Option<&str>,
    ) -> Result<(ApplicationConsent, ApplicationCredential), OrbitError> {
        if !cfg!(unix) {
            return Err(OrbitError::CapabilityDenied("application sessions require Unix inherited descriptors and private file permissions".into()));
        }
        // An agent cannot turn a trusted API or an operator environment into
        // application authority. Plugin callbacks cannot administer consent.
        let caller = CallerCapabilities::resolve(&CallerEnvelope::from_process_env(session));
        if agent_context_declared()
            || session.application.is_some()
            || !caller.grants().contains(&McpCapability::Operator)
            || orbit_tools::plugin::resolve_plugin_callback(&self.global_root())?.is_some()
        {
            return Err(OrbitError::CapabilityDenied(
                "application consent requires a host operator".into(),
            ));
        }
        if request.application_id.is_empty()
            || request.application_id.len() > 128
            || !request
                .application_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            || request.expires_at <= Utc::now()
            || request.scopes.is_empty()
            || request.scopes.len() > 64
        {
            return Err(OrbitError::InvalidInput(
                "application consent needs an application id, future expiry and 1..64 exact scopes"
                    .into(),
            ));
        }
        if serde_json::to_vec(&request)
            .map_err(|_| OrbitError::InvalidInput("invalid application consent".into()))?
            .len()
            > 4096
        {
            return Err(OrbitError::InvalidInput(
                "application consent exceeds the 4 KiB session scope budget".into(),
            ));
        }
        for scope in &request.scopes {
            self.validate_application_operation(&scope.operation)?;
            if !scope.callbacks.is_empty() && scope.operation.plugin.is_none() {
                return Err(OrbitError::InvalidInput(
                    "only plugin calls may have completion callbacks".into(),
                ));
            }
            for callback in &scope.callbacks {
                if callback.plugin.is_some() || !application_builtin_supported(&callback.tool) {
                    return Err(OrbitError::InvalidInput(
                        "completion callbacks must be supported built-in application operations"
                            .into(),
                    ));
                }
                self.validate_application_operation(callback)?;
            }
        }
        let credential = ApplicationCredential::mint()?;
        let consent = ApplicationConsent {
            schema_version: 1,
            consent_id: credential.consent_id().into(),
            application_id: request.application_id,
            workspace_id: self.workspace_id()?,
            workspace_root: self
                .paths()
                .repo_root
                .canonicalize()?
                .to_string_lossy()
                .into_owned(),
            consented_by: self.actor().label.clone(),
            consent_provenance: caller.provenance().to_string(),
            consented_at: Utc::now(),
            expires_at: request.expires_at,
            scopes: request.scopes,
        };
        let record = ApplicationConsentRecord {
            consent: consent.clone(),
            fingerprint: credential.fingerprint(),
            revoked: false,
        };
        let consent_json = serde_json::to_value(&consent)
            .map_err(|_| OrbitError::InvalidInput("invalid application consent".into()))?;
        if orbit_common::security::redaction::redact_sensitive_env_json(consent_json.clone())
            != consent_json
        {
            return Err(OrbitError::InvalidInput(
                "application consent must not contain credential values".into(),
            ));
        }
        // A protected witness prevents a writable SQLite row from creating
        // authority. Both halves must agree, including the secret fingerprint.
        if let Some(id) = replaces {
            self.revoke_application(id, session)?;
        }
        self.sqlite_store()?
            .insert_application_consent(&record, None)?;
        self.write_application_witness(&record)?;
        self.audit_application_lifecycle(
            &consent,
            if replaces.is_some() {
                "rotate"
            } else {
                "consent"
            },
        )?;
        Ok((consent, credential))
    }

    /// Revoke the consent and all callbacks already minted from it. In-flight
    /// mutations cannot be rolled back; the next admission always rechecks.
    pub fn revoke_application(
        &self,
        id: &str,
        session: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        let caller = CallerCapabilities::resolve(&CallerEnvelope::from_process_env(session));
        if agent_context_declared()
            || session.application.is_some()
            || !caller.grants().contains(&McpCapability::Operator)
            || orbit_tools::plugin::resolve_plugin_callback(&self.global_root())?.is_some()
        {
            return Err(OrbitError::CapabilityDenied(
                "application revocation requires a host operator".into(),
            ));
        }
        let store = self.sqlite_store()?;
        let record = store
            .get_application_consent(id)?
            .ok_or_else(|| OrbitError::CapabilityDenied("unknown application consent".into()))?;
        if record.consent.workspace_id != self.workspace_id()? {
            return Err(OrbitError::CapabilityDenied(
                "application consent belongs to another workspace".into(),
            ));
        }
        let path = self.application_witness_path(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                let parent = path.parent().ok_or_else(|| {
                    OrbitError::Execution("application consent directory is missing".into())
                })?;
                orbit_common::fs::io::sync_parent_dir(&std::fs::File::open(parent)?)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        store.revoke_application_consent(id, &self.workspace_id()?)?;
        self.audit_application_lifecycle(&record.consent, "revoke")
    }

    fn validate_application_operation(
        &self,
        operation: &ApplicationOperation,
    ) -> Result<(), OrbitError> {
        if !operation.input.is_object() {
            return Err(OrbitError::InvalidInput(
                "application operation input must be an exact JSON object".into(),
            ));
        }
        match self.tool_registry().plugin_binding(&operation.tool) {
            Some(binding) => {
                let expected = operation.plugin.as_ref().ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "plugin consent must name its installed manifest digest".into(),
                    )
                })?;
                if binding.provenance.name != expected.name
                    || binding.provenance.manifest_digest != expected.manifest_digest
                {
                    return Err(OrbitError::CapabilityDenied(
                        "plugin digest consent is stale".into(),
                    ));
                }
                let installed = self
                    .stores()
                    .plugins()
                    .get_plugin(&expected.name)?
                    .ok_or_else(|| {
                        OrbitError::CapabilityDenied("consented plugin is not installed".into())
                    })?;
                crate::plugin::grants::verify_install_path(&self.global_root(), &installed)
                    .map_err(OrbitError::CapabilityDenied)?;
                let loaded =
                    orbit_tools::plugin::load_plugin_dir(Path::new(&installed.install_path))
                        .map_err(|_| {
                            OrbitError::CapabilityDenied(
                                "consented plugin manifest cannot be verified".into(),
                            )
                        })?;
                if !installed.enabled || loaded.manifest_digest != expected.manifest_digest {
                    return Err(OrbitError::CapabilityDenied(
                        "plugin digest consent is stale or plugin disabled".into(),
                    ));
                }
            }
            None if operation.plugin.is_none()
                && application_builtin_supported(&operation.tool) =>
            {
                let keys: &[&str] = if operation.tool == "orbit.task.update" {
                    &["id", "status", "comment", "workspace"]
                } else {
                    &["id", "answer", "note", "workspace"]
                };
                if operation
                    .input
                    .as_object()
                    .is_none_or(|input| input.keys().any(|key| !keys.contains(&key.as_str())))
                    || operation
                        .input
                        .get("id")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                {
                    return Err(OrbitError::InvalidInput("application task decisions admit only id, status/answer, note, comment and workspace".into()));
                }
                if operation.tool == "orbit.task.update"
                    && operation.input.get("status").is_some_and(|status| {
                        !matches!(
                            status.as_str(),
                            Some("backlog" | "done" | "rejected" | "archived")
                        )
                    })
                {
                    return Err(OrbitError::InvalidInput("application task updates support approval, completion or dismissal statuses only".into()));
                }
            }
            _ => {
                return Err(OrbitError::CapabilityDenied(
                    "this built-in operation does not support application consent".into(),
                ));
            }
        }
        Ok(())
    }

    /// Resolve an inherited credential or an authenticated callback record,
    /// then check the exact workspace, input and live plugin digest. `false`
    /// means ordinary caller; any attempted invalid credential is a refusal.
    pub fn admit_application_call(
        &self,
        tool: &str,
        input: &Value,
        session: &mut ToolSessionContext,
        enforcement: CapabilityEnforcement,
    ) -> Result<bool, OrbitError> {
        let callback = orbit_tools::plugin::resolve_plugin_callback(&self.global_root())?;
        let inherited = callback
            .as_ref()
            .and_then(|identity| identity.application.as_deref().cloned());
        let mut invocation = if let Some(invocation) = inherited {
            Some(invocation)
        } else {
            session.application.as_deref().cloned()
        };
        if invocation.is_none()
            && enforcement == CapabilityEnforcement::Enforce
            && let Some(credential) = ApplicationCredential::from_process_descriptor()?
        {
            let record = self
                .sqlite_store()?
                .get_application_consent(credential.consent_id())?
                .filter(|record| credential.matches_fingerprint(&record.fingerprint))
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied("unrecognized application credential".into())
                })?;
            invocation = Some(ApplicationInvocation {
                consent: record.consent,
                scope_index: 0,
                callback: false,
            });
        }
        let Some(mut invocation) = invocation else {
            return Ok(false);
        };
        APPLICATION_AUDIT.with(|state| *state.borrow_mut() = Some(invocation.clone()));
        if agent_context_declared()
            || session.worker_invocation.is_some()
            || !session.effective_capabilities.is_empty()
        {
            return Err(OrbitError::CapabilityDenied(
                "agent and capability sessions cannot use application credentials".into(),
            ));
        }
        let record = self
            .sqlite_store()?
            .get_application_consent(&invocation.consent.consent_id)?
            .ok_or_else(|| OrbitError::CapabilityDenied("unknown application consent".into()))?;
        self.verify_application_witness(&record)?;
        if record.consent.schema_version != 1
            || record.revoked
            || record.consent != invocation.consent
            || record.consent.expires_at <= Utc::now()
        {
            return Err(OrbitError::CapabilityDenied(
                "application consent revoked, expired or replaced".into(),
            ));
        }
        if record.consent.workspace_id != self.workspace_id()?
            || Path::new(&record.consent.workspace_root) != self.paths().repo_root.canonicalize()?
        {
            return Err(OrbitError::CapabilityDenied(
                "application consent belongs to another workspace".into(),
            ));
        }
        if invocation.callback {
            let scope = record
                .consent
                .scopes
                .get(invocation.scope_index)
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied("invalid application callback scope".into())
                })?;
            self.validate_application_operation(&scope.operation)?;
            if !scope
                .callbacks
                .iter()
                .any(|op| op.tool == tool && op.input == *input)
            {
                return Err(OrbitError::CapabilityDenied(
                    "completion callback is outside application consent".into(),
                ));
            }
        } else {
            invocation.scope_index = record
                .consent
                .scopes
                .iter()
                .position(|scope| scope.operation.tool == tool && scope.operation.input == *input)
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied(
                        "operation or input is outside application consent".into(),
                    )
                })?;
            self.validate_application_operation(
                &record.consent.scopes[invocation.scope_index].operation,
            )?;
        }
        APPLICATION_AUDIT.with(|state| *state.borrow_mut() = Some(invocation.clone()));
        session.application = Some(Box::new(invocation));
        Ok(true)
    }

    fn application_witness_path(&self, id: &str) -> Result<std::path::PathBuf, OrbitError> {
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(OrbitError::CapabilityDenied(
                "invalid application consent id".into(),
            ));
        }
        let root = self.global_root().canonicalize()?;
        let mut parent = root.clone();
        for segment in ["state", "application-consents"] {
            parent.push(segment);
            match std::fs::symlink_metadata(&parent) {
                Ok(metadata) => {
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err(OrbitError::CapabilityDenied(
                            "application authority directory must not be a symlink".into(),
                        ));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        // SAFETY: geteuid has no preconditions.
                        if metadata.uid() != unsafe { libc::geteuid() }
                            || metadata.mode() & 0o022 != 0
                        {
                            return Err(OrbitError::CapabilityDenied("application authority directory has unsafe ownership or permissions".into()));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(parent.join(format!("{id}.json")))
    }

    fn audit_application_lifecycle(
        &self,
        consent: &ApplicationConsent,
        action: &str,
    ) -> Result<(), OrbitError> {
        use orbit_store::contracts::{AuditEventInsertParams, AuditInvocationFields};
        let invocation = ApplicationInvocation {
            consent: consent.clone(),
            scope_index: 0,
            callback: false,
        };
        self.record_audit_event_with_invocation(
            &AuditEventInsertParams {
                execution_id: orbit_common::observability::audit_id::audit_execution_id(
                    "application",
                ),
                command: "application".into(),
                subcommand: Some(action.into()),
                tool_name: None,
                target_type: Some("application_consent".into()),
                target_id: Some(consent.consent_id.clone()),
                role: self.actor().audit_role().into(),
                status: orbit_types::telemetry::AuditEventStatus::Success,
                exit_code: 0,
                duration_ms: 1,
                working_directory: self.paths().repo_root.to_string_lossy().into_owned(),
                arguments_json: None,
                stdout_truncated: None,
                stderr_truncated: None,
                error_message: None,
                host: None,
                pid: std::process::id(),
                session_id: None,
                workspace_id: Some(consent.workspace_id.clone()),
                caller_machine_id: None,
                caller_machine_name: None,
                process_machine_id: None,
                process_machine_name: None,
                transport: None,
                effective_capabilities: Default::default(),
                origin_session_id: None,
                mcp_call_id: None,
                lease_id: None,
                task_id: None,
                job_run_id: None,
                activity_id: None,
                step_index: None,
            },
            AuditInvocationFields {
                application: Some(&invocation),
                ..Default::default()
            },
        )
    }

    fn write_application_witness(
        &self,
        record: &ApplicationConsentRecord,
    ) -> Result<(), OrbitError> {
        let path = self.application_witness_path(&record.consent.consent_id)?;
        let parent = path.parent().ok_or_else(|| {
            OrbitError::Execution("application consent directory is missing".into())
        })?;
        orbit_common::fs::io::create_private_dir_all(parent)?;
        let json =
            serde_json::json!({"consent": record.consent, "fingerprint": record.fingerprint});
        orbit_common::fs::io::write_new_private_text(&path, &json.to_string())?;
        Ok(())
    }

    fn verify_application_witness(
        &self,
        record: &ApplicationConsentRecord,
    ) -> Result<(), OrbitError> {
        let path = self.application_witness_path(&record.consent.consent_id)?;
        let refused = || {
            OrbitError::CapabilityDenied(
                "application consent witness is unavailable or invalid".into(),
            )
        };
        let mut file =
            orbit_common::fs::io::open_read_only_no_follow(&path).map_err(|_| refused())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata().map_err(|_| refused())?;
            // SAFETY: geteuid has no memory safety preconditions.
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
                return Err(refused());
            }
        }
        use std::io::Read;
        let mut raw = String::new();
        file.by_ref()
            .take(1024 * 1024)
            .read_to_string(&mut raw)
            .map_err(|_| refused())?;
        let actual: Value = serde_json::from_str(&raw).map_err(|_| refused())?;
        if actual
            != serde_json::json!({"consent": record.consent, "fingerprint": record.fingerprint})
        {
            return Err(refused());
        }
        Ok(())
    }
}
