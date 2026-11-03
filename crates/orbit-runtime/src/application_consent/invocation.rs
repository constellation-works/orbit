//! Per-call admission, invocation audit, rate charging and credential witnesses.

use super::admission::{
    CallbackBindings, ConstrainedAdmission, ConstraintRefusal, admit_within_rate_limit,
    evaluate_constrained,
};
use super::validation::OperationRole;
use crate::{RuntimeKernel, tool_exec::CapabilityEnforcement};
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::security::application_credential::ApplicationCredential;
use orbit_store::contracts::{ApplicationConsentRecord, ApplicationConsentStoreBackend};
use orbit_types::tool::{
    ApplicationAdmissionMatch, ApplicationAuditRefusal, ApplicationInvocation, ToolSessionContext,
};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

thread_local! {
    static APPLICATION_AUDIT: RefCell<Option<ApplicationInvocation>> = const { RefCell::new(None) };
    /// Calls this thread already charged to a scope rate limit. One call is
    /// admitted at several chokepoints (authorization, execution, the audited
    /// dispatch); it is charged once, at the first.
    static RATE_LIMIT_CHARGED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// Drain the host-authenticated facts for the enclosing dispatch's audit row.
pub fn take_application_audit() -> Option<ApplicationInvocation> {
    APPLICATION_AUDIT.with(|state| state.borrow_mut().take())
}

/// End of one audited dispatch: the next call is charged to its scope's rate
/// limit afresh.
pub fn finish_application_dispatch() {
    RATE_LIMIT_CHARGED.with(|charged| charged.borrow_mut().clear());
}

/// Record the first constrained refusal on the audit slot filled at the
/// start of admission. Bindings, text digests and predicate evidence already
/// on the slot stay: a callback refusal must not drop the entry's evidence,
/// and a failed admission's own values are never copied onto it.
fn stamp_application_refusal(
    scope_index: usize,
    scope_name: Option<String>,
    field: Option<String>,
    constraint: &str,
    constrained: bool,
) {
    APPLICATION_AUDIT.with(|state| {
        let mut slot = state.borrow_mut();
        let Some(invocation) = slot.as_mut() else {
            return;
        };
        invocation.scope_index = scope_index;
        invocation.scope_name.clone_from(&scope_name);
        invocation.admission_match = if constrained {
            ApplicationAdmissionMatch::Constrained
        } else {
            ApplicationAdmissionMatch::Exact
        };
        invocation.refused = Some(ApplicationAuditRefusal {
            scope_name,
            field,
            constraint: constraint.to_owned(),
        });
    });
}

impl RuntimeKernel {
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
            invocation = Some(ApplicationInvocation::new(record.consent, 0, false));
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
        if !matches!(
            record.consent.schema_version,
            orbit_types::tool::APPLICATION_CONSENT_SCHEMA_V1
                | orbit_types::tool::APPLICATION_CONSENT_SCHEMA_V2
        ) || record.revoked
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
        // Set from the callback that matched, not the first one of this
        // tool. An exact callback of a constrained entry stays exact.
        let mut matched_callback_constrained = None;
        if invocation.callback {
            let scope = record
                .consent
                .scopes
                .get(invocation.scope_index)
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied("invalid application callback scope".into())
                })?;
            self.validate_application_operation(&scope.operation, OperationRole::Entry)?;
            // Every callback rechecks the payload the entry was admitted on.
            self.recheck_bound_application_artifact(&invocation)?;
            let mut refusals = Vec::new();
            let mut first_refusal: Option<ConstraintRefusal> = None;
            let mut matched = None;
            for callback in scope.callbacks.iter().filter(|op| op.tool == tool) {
                match (&callback.input, &callback.fields) {
                    (Some(exact), None) if exact == input => {
                        matched = Some((callback, ConstrainedAdmission::default()));
                        break;
                    }
                    (None, Some(fields)) => {
                        let predicate = invocation.predicate.as_ref();
                        let bindings = CallbackBindings {
                            bindings: &invocation.bindings,
                            pinned_task: invocation.pinned_task.as_deref(),
                            bound_task: predicate.map(|predicate| predicate.task.as_str()),
                            bound_artifact: predicate
                                .and_then(|predicate| predicate.artifact.as_ref()),
                        };
                        match evaluate_constrained(
                            self,
                            &record.consent,
                            fields,
                            input,
                            Some(bindings),
                        )? {
                            Ok(admission) => {
                                matched = Some((callback, admission));
                                break;
                            }
                            Err(refusal) => {
                                if first_refusal.is_none() {
                                    first_refusal = Some(refusal.clone());
                                }
                                refusals.push(refusal.describe());
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some((callback, admission)) = matched else {
                if let Some(refusal) = first_refusal {
                    stamp_application_refusal(
                        invocation.scope_index,
                        scope.name.clone(),
                        refusal.field,
                        refusal.constraint,
                        true,
                    );
                }
                return Err(OrbitError::CapabilityDenied(if refusals.is_empty() {
                    "completion callback is outside application consent".into()
                } else {
                    format!(
                        "completion callback is outside application consent: {}",
                        refusals.join("; ")
                    )
                }));
            };
            self.validate_application_operation(
                callback,
                OperationRole::Callback {
                    entry: &scope.operation,
                },
            )?;
            if let Some(task) = admission.pin {
                orbit_tools::plugin::pin_routed_application_task(&task)?;
                invocation.pinned_task = Some(task);
            }
            let constrained = callback.is_constrained();
            if constrained {
                invocation.text_digests = admission.text_digests;
            }
            matched_callback_constrained = Some(constrained);
        } else {
            let scopes = &record.consent.scopes;
            let mut refusals = Vec::new();
            let mut first_refusal: Option<(usize, Option<String>, ConstraintRefusal)> = None;
            let mut matched = None;
            for (index, scope) in scopes.iter().enumerate() {
                if scope.operation.tool != tool {
                    continue;
                }
                match (&scope.operation.input, &scope.operation.fields) {
                    (Some(exact), None) if exact == input => {
                        matched = Some((index, ConstrainedAdmission::default()));
                        break;
                    }
                    (None, Some(fields)) => {
                        match evaluate_constrained(self, &record.consent, fields, input, None)? {
                            Ok(admission) => {
                                matched = Some((index, admission));
                                break;
                            }
                            Err(refusal) => {
                                if first_refusal.is_none() {
                                    first_refusal =
                                        Some((index, scope.name.clone(), refusal.clone()));
                                }
                                refusals.push(format!(
                                    "scope `{}` {}",
                                    scope.name.as_deref().unwrap_or("unnamed"),
                                    refusal.describe()
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some((index, admission)) = matched else {
                // Name each constrained scope's failing field and constraint
                // kind; never a value the call sent.
                if let Some((index, scope_name, refusal)) = first_refusal {
                    stamp_application_refusal(
                        index,
                        scope_name,
                        refusal.field,
                        refusal.constraint,
                        true,
                    );
                }
                return Err(OrbitError::CapabilityDenied(if refusals.is_empty() {
                    "operation or input is outside application consent".into()
                } else {
                    format!(
                        "operation or input is outside application consent: {}",
                        refusals.join("; ")
                    )
                }));
            };
            self.validate_application_operation(&scopes[index].operation, OperationRole::Entry)?;
            if let Some(limit) = &scopes[index].rate_limit
                && !self.charge_application_rate_limit(
                    &record.consent.consent_id,
                    index,
                    limit,
                    tool,
                    input,
                )?
            {
                stamp_application_refusal(
                    index,
                    scopes[index].name.clone(),
                    None,
                    "rate_limit",
                    scopes[index].operation.is_constrained(),
                );
                return Err(OrbitError::CapabilityDenied(format!(
                    "application scope `{}` reached its rate limit of {} admissions per {} seconds",
                    scopes[index].name.as_deref().unwrap_or("unnamed"),
                    limit.admissions,
                    limit.window_seconds
                )));
            }
            invocation.scope_index = index;
            invocation.bindings = admission.bindings;
            invocation.text_digests = admission.text_digests;
            invocation.predicate = admission.predicate;
            invocation.admission_match = if scopes[index].operation.is_constrained() {
                ApplicationAdmissionMatch::Constrained
            } else {
                ApplicationAdmissionMatch::Exact
            };
        }
        invocation.scope_name = record
            .consent
            .scopes
            .get(invocation.scope_index)
            .and_then(|scope| scope.name.clone());
        if let Some(constrained) = matched_callback_constrained {
            invocation.admission_match = if constrained {
                ApplicationAdmissionMatch::Constrained
            } else {
                ApplicationAdmissionMatch::Exact
            };
        }
        APPLICATION_AUDIT.with(|state| *state.borrow_mut() = Some(invocation.clone()));
        session.application = Some(Box::new(invocation));
        Ok(true)
    }

    /// Refuse a callback once the artifact the entry's `task` predicate bound
    /// no longer has the digest the entry was admitted on.
    fn recheck_bound_application_artifact(
        &self,
        invocation: &ApplicationInvocation,
    ) -> Result<(), OrbitError> {
        let Some(evidence) = &invocation.predicate else {
            return Ok(());
        };
        let Some(artifact) = &evidence.artifact else {
            return Ok(());
        };
        match self.task_artifact_sha256(&evidence.task, &artifact.path) {
            Ok(Some(actual)) if actual == artifact.sha256 => Ok(()),
            _ => Err(OrbitError::CapabilityDenied(
                "bound artifact changed since the application entry was admitted".into(),
            )),
        }
    }

    /// Charge one call to a scope's rate limit; `false` when the window is
    /// full. A call this thread already charged within the current dispatch
    /// passes without a second charge.
    fn charge_application_rate_limit(
        &self,
        consent_id: &str,
        scope_index: usize,
        limit: &orbit_types::tool::ApplicationRateLimit,
        tool: &str,
        input: &Value,
    ) -> Result<bool, OrbitError> {
        let key = format!(
            "{consent_id}:{scope_index}:{}",
            orbit_common::security::release::sha256_hex(format!("{tool}\0{input}").as_bytes())
        );
        if RATE_LIMIT_CHARGED.with(|charged| charged.borrow().contains(&key)) {
            return Ok(true);
        }
        let admitted = admit_within_rate_limit(
            &self.application_admission_log_path(consent_id)?,
            scope_index,
            limit,
            Utc::now(),
        )?;
        if admitted {
            RATE_LIMIT_CHARGED.with(|charged| charged.borrow_mut().insert(key));
        }
        Ok(admitted)
    }

    /// The scope rate-limit log, beside the consent's witness.
    fn application_admission_log_path(&self, id: &str) -> Result<std::path::PathBuf, OrbitError> {
        let path = self.application_witness_path(id)?;
        if let Some(parent) = path.parent() {
            orbit_common::fs::io::create_private_dir_all(parent)?;
        }
        Ok(path.with_file_name(format!("{id}.admissions.json")))
    }

    pub(super) fn application_witness_path(
        &self,
        id: &str,
    ) -> Result<std::path::PathBuf, OrbitError> {
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

    pub(super) fn write_application_witness(
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
