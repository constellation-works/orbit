//! Grant lifecycle: explicit scoped enablement, ordinary stop, hard
//! revocation [ORB-11332].
//!
//! Enablement is the one moment authority is created. It validates the
//! finite scope against live tasks, resolves the effective policy once
//! (built-in → global → workspace → this request's run layer), refuses an
//! explicit escalation the repository cap forbids, captures the numeric
//! limits, and persists the grant. Every later privileged action rechecks
//! that record; editing a preference afterwards changes nothing about it.

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_config::{CompletionPreference, DeliveryCap, OperationLayer};
use orbit_store::contracts::{
    GrantInsertOutcome, GrantTransitionKind, GrantTransitionOutcome, GrantTransitionRequest,
};
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    GrantLimits, GrantRights, GrantStatus, MAX_GRANT_SCOPE_TASKS, MAX_GRANT_WINDOW_SECONDS,
    OperationGrant,
};
use serde_json::{Value, json};

use super::{ENABLE_OPERATION, GRANT_AUDIT, REVOKE_OPERATION, STOP_OPERATION};
use crate::OrbitRuntime;
use crate::application::job::DrainAdmissionsStopChange;

/// One request to enable a scoped grant.
#[derive(Debug, Clone)]
pub struct EnableOperationGrantRequest<'a> {
    /// The finite task set. Duplicates collapse; every id must name a
    /// proposed or backlog task in this workspace.
    pub task_ids: &'a [String],
    /// Admission window from now, in seconds (1..=24h).
    pub window_seconds: u64,
    /// Which rights the grant carries. At least one is required.
    pub rights: GrantRights,
    /// Explicit run-layer overrides for this enablement.
    pub run_layer: OperationLayer,
    pub actor: &'a str,
    pub source: &'a str,
    pub claim_token: Option<&'a str>,
}

/// One request to stop or revoke a grant.
#[derive(Debug, Clone)]
pub struct OperationGrantControlRequest<'a> {
    /// The grant to change; `None` selects the workspace's active grant.
    pub grant_id: Option<&'a str>,
    pub reason: Option<&'a str>,
    /// Compare-and-set expectation; a mismatch refuses the write.
    pub expected_revision: Option<u32>,
    pub actor: &'a str,
    pub source: &'a str,
    pub claim_token: Option<&'a str>,
}

/// What a stop or revocation did.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationGrantControlResult {
    pub grant: OperationGrant,
    /// `stopped`, `revoked`, or `unchanged`.
    pub outcome: &'static str,
    /// Coordinators bound to this grant whose admissions were stopped.
    pub coordinators: Vec<DrainAdmissionsStopChange>,
}

impl OrbitRuntime {
    /// Enable a scoped grant. Preferences are resolved and captured once
    /// here; the grant is the durable authorization.
    pub fn enable_operation_grant(
        &self,
        request: EnableOperationGrantRequest<'_>,
    ) -> Result<OperationGrant, OrbitError> {
        self.require_workspace_claim(ENABLE_OPERATION, request.claim_token)?;
        let request_id = audit_execution_id("operation_enable");
        let outcome = self.build_and_insert_grant(&request);
        match &outcome {
            Ok(grant) => self.record_grant_audit(
                &request_id,
                Some(&grant.id),
                request.actor,
                "enabled",
                json!({
                    "task_ids": grant.task_ids,
                    "rights": grant.rights.names(),
                    "expires_at": grant.expires_at.to_rfc3339(),
                    "limits": grant.limits,
                    "policy_version": grant.policy_version,
                    "source": request.source,
                }),
                None,
            )?,
            Err(error) => self.record_grant_audit(
                &request_id,
                None,
                request.actor,
                "rejected",
                json!({
                    "task_ids": request.task_ids,
                    "rights": request.rights.names(),
                    "window_seconds": request.window_seconds,
                    "source": request.source,
                }),
                Some(error.to_string()),
            )?,
        }
        outcome
    }

    fn build_and_insert_grant(
        &self,
        request: &EnableOperationGrantRequest<'_>,
    ) -> Result<OperationGrant, OrbitError> {
        let task_ids = self.validated_grant_scope(request.task_ids)?;
        if request.window_seconds == 0 || request.window_seconds > MAX_GRANT_WINDOW_SECONDS {
            return Err(OrbitError::InvalidInput(format!(
                "grant window must be between 1 and {MAX_GRANT_WINDOW_SECONDS} seconds"
            )));
        }
        if request.rights.names().is_empty() {
            return Err(OrbitError::InvalidInput(
                "a grant needs at least one right: prepare, promote, or complete".to_string(),
            ));
        }

        let policy = self.operation_policy().with_run_layer(&request.run_layer);
        // [ORB-11333] `before-pr` is captured like any other review timing;
        // the gate itself checks the reviewer crew at admission of each run.
        // An explicit escalation past the repository cap is refused rather
        // than silently reduced; a configured preference is capped and the
        // cap is disclosed by the captured policy.
        if policy.delivery_cap.value == DeliveryCap::Review {
            if request.run_layer.completion == Some(CompletionPreference::Done) {
                return Err(OrbitError::InvalidInput(
                    "explicit completion 'done' exceeds the repository delivery cap 'review'; \
                     raise operation.delivery_cap in the workspace config first"
                        .to_string(),
                ));
            }
            if request.rights.complete {
                return Err(OrbitError::InvalidInput(
                    "the complete right exceeds the repository delivery cap 'review'; raise \
                     operation.delivery_cap in the workspace config or omit the right"
                        .to_string(),
                ));
            }
        }

        let hard_limit = self.leaf_run_hard_limit()?;
        let now = Utc::now();
        let grant = OperationGrant {
            id: audit_execution_id("ogrant"),
            workspace_id: self.workspace_id()?,
            actor: request.actor.to_string(),
            source: request.source.to_string(),
            created_at: now,
            expires_at: now
                + Duration::seconds(i64::try_from(request.window_seconds).unwrap_or(i64::MAX)),
            revision: 1,
            task_ids,
            rights: request.rights,
            limits: GrantLimits {
                leaf_ceiling: policy.leaf_ceiling.value.min(hard_limit).max(1),
                preparation_due_seconds: policy.preparation_due_seconds.value,
                recovery_episodes_per_task: policy.recovery_episodes_per_task.value,
                recovery_minutes_per_task: policy.recovery_minutes_per_task.value,
            },
            policy: serde_json::to_value(&policy).map_err(|error| {
                OrbitError::Execution(format!("serialize operation policy: {error}"))
            })?,
            policy_version: policy.version,
            status: GrantStatus::Active,
            stopped: None,
            revoked: None,
        };

        match self.operation_store()?.operation_grant_insert(&grant)? {
            GrantInsertOutcome::Inserted => Ok(grant),
            GrantInsertOutcome::ActiveGrantExists(existing) => {
                Err(OrbitError::InvalidInput(format!(
                    "this workspace already has active operation grant '{existing}'; stop it \
                     before enabling a replacement window"
                )))
            }
        }
    }

    /// Canonical finite scope: sorted, deduplicated, bounded, and every id a
    /// live proposed or backlog task.
    fn validated_grant_scope(&self, task_ids: &[String]) -> Result<Vec<String>, OrbitError> {
        let mut ids = task_ids
            .iter()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return Err(OrbitError::InvalidInput(
                "a grant needs a finite, non-empty task set".to_string(),
            ));
        }
        if ids.len() > MAX_GRANT_SCOPE_TASKS {
            return Err(OrbitError::InvalidInput(format!(
                "a grant may name at most {MAX_GRANT_SCOPE_TASKS} tasks; enable a second grant \
                 for a larger rollout"
            )));
        }
        for id in &ids {
            let task = self.get_task(id)?;
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                return Err(OrbitError::InvalidInput(format!(
                    "task {id} is {}; a grant covers proposed or backlog work only",
                    task.status
                )));
            }
        }
        Ok(ids)
    }

    /// Stop new admissions and promotion under a grant. Admitted work keeps
    /// its captured bounds, including completion.
    pub fn stop_operation_grant(
        &self,
        request: OperationGrantControlRequest<'_>,
    ) -> Result<OperationGrantControlResult, OrbitError> {
        self.require_workspace_claim(STOP_OPERATION, request.claim_token)?;
        self.transition_grant(GrantTransitionKind::Stop, request)
    }

    /// Hard revocation: withdraw privileged actions from admitted work too.
    pub fn revoke_operation_grant(
        &self,
        request: OperationGrantControlRequest<'_>,
    ) -> Result<OperationGrantControlResult, OrbitError> {
        self.require_workspace_claim(REVOKE_OPERATION, request.claim_token)?;
        self.transition_grant(GrantTransitionKind::Revoke, request)
    }

    fn transition_grant(
        &self,
        kind: GrantTransitionKind,
        request: OperationGrantControlRequest<'_>,
    ) -> Result<OperationGrantControlResult, OrbitError> {
        let label = match kind {
            GrantTransitionKind::Stop => "stop",
            GrantTransitionKind::Revoke => "revoke",
        };
        let request_id = audit_execution_id(&format!("operation_{label}"));
        let outcome = self.apply_grant_transition(kind, &request);
        match &outcome {
            Ok(result) => self.record_grant_audit(
                &request_id,
                Some(&result.grant.id),
                request.actor,
                result.outcome,
                json!({
                    "kind": label,
                    "reason": request.reason,
                    "revision": result.grant.revision,
                    "status": result.grant.status.as_str(),
                    "coordinators": result
                        .coordinators
                        .iter()
                        .map(|change| json!({ "run_id": change.run_id, "outcome": change.outcome }))
                        .collect::<Vec<_>>(),
                    "source": request.source,
                }),
                None,
            )?,
            Err(error) => self.record_grant_audit(
                &request_id,
                request.grant_id,
                request.actor,
                "rejected",
                json!({ "kind": label, "reason": request.reason, "source": request.source }),
                Some(error.to_string()),
            )?,
        }
        outcome
    }

    fn apply_grant_transition(
        &self,
        kind: GrantTransitionKind,
        request: &OperationGrantControlRequest<'_>,
    ) -> Result<OperationGrantControlResult, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let store = self.operation_store()?;
        let grant_id = match request.grant_id {
            Some(id) => id.to_string(),
            None => store
                .operation_active_grant(&workspace_id, Utc::now())?
                .or(store.operation_grants(&workspace_id, 1)?.into_iter().next())
                .map(|grant| grant.id)
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "this workspace has no operation grant to change".to_string(),
                    )
                })?,
        };

        let (grant, changed) = match store.operation_grant_transition(
            &workspace_id,
            &grant_id,
            &GrantTransitionRequest {
                kind,
                actor: request.actor,
                reason: request.reason,
                expected_revision: request.expected_revision,
                now: Utc::now(),
            },
        )? {
            GrantTransitionOutcome::Applied(grant) => (grant, true),
            GrantTransitionOutcome::Unchanged(grant) => (grant, false),
            GrantTransitionOutcome::RevisionConflict(grant) => {
                return Err(OrbitError::JobRunControlConflict(format!(
                    "operation grant '{}' is at revision {}, not the expected {}; reread and retry",
                    grant.id,
                    grant.revision,
                    request.expected_revision.unwrap_or_default()
                )));
            }
            GrantTransitionOutcome::NotFound => {
                return Err(OrbitError::InvalidInput(format!(
                    "operation grant '{grant_id}' was not found in this workspace"
                )));
            }
        };

        // Either transition ends new admissions, so the drains this grant
        // admitted are told to stop offering work. Their children keep
        // running; revocation is enforced where privileged actions happen.
        let coordinators =
            self.stop_grant_bound_drains(&grant.id, request.actor, request.source, request.reason)?;
        let outcome = match (changed, kind) {
            (false, _) => "unchanged",
            (true, GrantTransitionKind::Stop) => "stopped",
            (true, GrantTransitionKind::Revoke) => "revoked",
        };
        Ok(OperationGrantControlResult {
            grant,
            outcome,
            coordinators,
        })
    }

    pub(crate) fn record_grant_audit(
        &self,
        request_id: &str,
        grant_id: Option<&str>,
        actor: &str,
        outcome: &str,
        detail: Value,
        error: Option<String>,
    ) -> Result<(), OrbitError> {
        let mut arguments = json!({
            "request_id": request_id,
            "grant_id": grant_id,
            "outcome": outcome,
            "actor": actor,
            "recorded_at": Utc::now().to_rfc3339(),
        });
        if let (Some(target), Some(detail)) = (arguments.as_object_mut(), detail.as_object()) {
            for (key, value) in detail {
                target.insert(key.clone(), value.clone());
            }
        }
        self.record_pipeline_audit(
            GRANT_AUDIT,
            grant_id,
            Some(actor),
            if outcome == "rejected" {
                AuditEventStatus::Failure
            } else {
                AuditEventStatus::Success
            },
            arguments,
            error,
        )
    }
}
