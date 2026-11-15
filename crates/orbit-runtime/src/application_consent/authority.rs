//! Operator consent, revocation and lifecycle audit.

use super::validation::{OperationRole, validate_consent_request_v2};
use crate::RuntimeKernel;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, agent_context_declared,
};
use orbit_common::security::application_credential::ApplicationCredential;
use orbit_store::contracts::{ApplicationConsentRecord, ApplicationConsentStoreBackend};
use orbit_types::tool::{
    ApplicationConsent, ApplicationConsentRequest, ApplicationInvocation, McpCapability,
    ToolSessionContext,
};
use std::path::Path;

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
                "application consent needs an application id, future expiry and 1..64 scopes"
                    .into(),
            ));
        }
        let schema_version = request.schema_version();
        if schema_version == orbit_types::tool::APPLICATION_CONSENT_SCHEMA_V1
            && serde_json::to_vec(&request)
                .map_err(|_| OrbitError::InvalidInput("invalid application consent".into()))?
                .len()
                > 4096
        {
            return Err(OrbitError::InvalidInput(
                "application consent exceeds the 4 KiB session scope budget".into(),
            ));
        }
        validate_consent_request_v2(
            &request,
            super::validation::APPLICATION_CONSENT_MAX_DAYS,
            Utc::now(),
        )?;
        for scope in &request.scopes {
            self.validate_application_operation(&scope.operation, OperationRole::Entry)?;
            if !scope.callbacks.is_empty() && scope.operation.plugin.is_none() {
                return Err(OrbitError::InvalidInput(
                    "only plugin calls may have completion callbacks".into(),
                ));
            }
            for callback in &scope.callbacks {
                if callback.plugin.is_some()
                    || self
                        .tool_registry()
                        .plugin_binding(&callback.tool)
                        .is_some()
                {
                    return Err(OrbitError::InvalidInput(
                        "completion callbacks must be supported built-in application operations"
                            .into(),
                    ));
                }
                self.validate_application_operation(
                    callback,
                    OperationRole::Callback {
                        entry: &scope.operation,
                    },
                )?;
            }
        }
        let credential = ApplicationCredential::mint()?;
        let consent = ApplicationConsent {
            schema_version,
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

    fn audit_application_lifecycle(
        &self,
        consent: &ApplicationConsent,
        action: &str,
    ) -> Result<(), OrbitError> {
        use orbit_store::contracts::{AuditEventInsertParams, AuditInvocationFields};
        let invocation = ApplicationInvocation::new(consent.clone(), 0, false);
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
}
