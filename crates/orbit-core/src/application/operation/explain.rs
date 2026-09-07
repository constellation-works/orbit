//! The effective-policy explanation: every resolved field with its winning
//! source, the authority actually in force, and each cap or limiting reason
//! that reduces requested behavior [ORB-11332].
//!
//! This is a read-only projection over configuration, the grant store, the
//! live drain, and the workspace's routine definitions. It never schedules,
//! promotes, or authorizes anything.

use chrono::Utc;
use orbit_automation::routines::loader::{RoutineSource, collect_routines};
use orbit_common::OrbitError;
use orbit_config::{OperationLayer, PreparationPreference, RecoveryPreference, ReviewPolicy};
use orbit_types::workflow::OperationAdmission;
use orbit_types::workflow::automation::members::StateTriggerKind;
use serde_json::{Value, json};

use super::NO_GRANT_REASON;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Explain the effective operation policy, optionally with run-layer
    /// overrides applied as a preview.
    pub fn explain_operation(
        &self,
        run_layer: Option<&OperationLayer>,
    ) -> Result<Value, OrbitError> {
        let policy = match run_layer {
            Some(layer) => self.operation_policy().with_run_layer(layer),
            None => self.operation_policy().clone(),
        };
        let now = Utc::now();
        let hard_limit = self.leaf_run_hard_limit()?;
        let grant = self.active_operation_grant()?;
        let mut limiting_reasons = Vec::new();

        let (effective_completion, cap_reason) = policy.capped_completion();
        if let Some(reason) = cap_reason {
            limiting_reasons.push(reason.to_string());
        }
        let leaf_cap = (policy.leaf_ceiling.value > hard_limit).then_some("job_hard_limit");
        if let Some(reason) = leaf_cap {
            limiting_reasons.push(reason.to_string());
        }

        let authority = match &grant {
            Some(grant) => {
                let admission = grant.admission(now);
                json!({
                    "grant_id": grant.id,
                    "status": grant.status.as_str(),
                    "admission": admission.reason(),
                    "task_ids": grant.task_ids,
                    "rights": grant.rights.names(),
                    "expires_at": grant.expires_at.to_rfc3339(),
                    "remaining_seconds": grant.remaining_seconds(now),
                    "revision": grant.revision,
                    "limits": grant.limits,
                    "policy_version": grant.policy_version,
                    "actor": grant.actor,
                    "created_at": grant.created_at.to_rfc3339(),
                })
            }
            None => {
                limiting_reasons.push(NO_GRANT_REASON.to_string());
                json!({ "grant_id": null, "admission": "none", "reason": NO_GRANT_REASON })
            }
        };

        let routines = self.state_routine_owners();
        let preparation_owners = routines
            .iter()
            .filter(|(kind, _, _)| *kind == StateTriggerKind::PreparationEligible)
            .map(|(_, name, enabled)| json!({ "routine": name, "enabled": enabled }))
            .collect::<Vec<_>>();
        let triage_owners = routines
            .iter()
            .filter(|(kind, _, _)| *kind == StateTriggerKind::ExecutionFailed)
            .map(|(_, name, enabled)| json!({ "routine": name, "enabled": enabled }))
            .collect::<Vec<_>>();
        let preparation_reason = if policy.preparation.value == PreparationPreference::Automatic
            && !preparation_owners
                .iter()
                .any(|owner| owner["enabled"] == true)
        {
            limiting_reasons.push("no_enabled_preparation_routine".to_string());
            Some("no_enabled_preparation_routine")
        } else {
            None
        };
        let recovery_reason = if policy.recovery.value == RecoveryPreference::Scheduled
            && !triage_owners.iter().any(|owner| owner["enabled"] == true)
        {
            limiting_reasons.push("no_enabled_triage_routine".to_string());
            Some("no_enabled_triage_routine")
        } else {
            None
        };
        // [ORB-11333] A before-PR gate needs an explicitly configured
        // reviewer crew; without one every gated delivery escalates.
        let review_reason = (policy.review_policy.value == ReviewPolicy::BeforePr
            && policy.review_crew.value.is_none())
        .then(|| {
            limiting_reasons.push("review_crew_unconfigured".to_string());
            "review_crew_unconfigured"
        });

        let drain = self.explain_live_drain()?;

        Ok(json!({
            "policy": policy.explain(),
            "authority": authority,
            "limits": {
                "leaf_ceiling": {
                    "preference": policy.leaf_ceiling.value,
                    "hard_limit": hard_limit,
                    "effective": policy.leaf_ceiling.value.min(hard_limit),
                    "cap": leaf_cap,
                },
                "window_max_seconds": orbit_types::workflow::MAX_GRANT_WINDOW_SECONDS,
                "scope_max_tasks": orbit_types::workflow::MAX_GRANT_SCOPE_TASKS,
            },
            "delivery": {
                "completion_preference": policy.completion.value,
                "delivery_cap": policy.delivery_cap.value,
                "effective_completion": effective_completion,
                "cap": cap_reason,
                "grant_complete_right": grant.as_ref().map(|grant| grant.rights.complete),
            },
            "preparation": {
                "preference": policy.preparation.value,
                "due_seconds": policy.preparation_due_seconds.value,
                "cadence_owners": preparation_owners,
                "reason": preparation_reason,
            },
            "recovery": {
                "preference": policy.recovery.value,
                "episodes_per_task": policy.recovery_episodes_per_task.value,
                "minutes_per_task": policy.recovery_minutes_per_task.value,
                "triage_owners": triage_owners,
                "reason": recovery_reason,
            },
            "review": {
                "policy": policy.review_policy.value,
                "policy_source": policy.review_policy.source.label(),
                "gates_pr": policy.review_policy.value == ReviewPolicy::BeforePr,
                "crew": policy.review_crew.value,
                "crew_source": policy.review_crew.source.label(),
                "budget": policy.review_budget(),
                "contract_version": orbit_types::workflow::REVIEW_CONTRACT_VERSION,
                "reason": review_reason,
            },
            "drain": drain,
            "limiting_reasons": limiting_reasons,
        }))
    }

    /// Enabled/disabled state routines in this workspace's routine directory,
    /// by trigger kind. The routine is the single cadence owner; operation
    /// mode only supplies constraints to it.
    fn state_routine_owners(&self) -> Vec<(StateTriggerKind, String, bool)> {
        let collection = collect_routines(
            &[RoutineSource {
                workspace: "workspace".to_string(),
                orbit_dir: self.shared_root(),
                enabled: true,
            }],
            &|_, _| true,
            self.automation_machine_identity().unwrap_or_default(),
        );
        collection
            .routines
            .iter()
            .filter_map(|routine| {
                routine.definition.trigger.state.as_ref().map(|state| {
                    (
                        state.kind,
                        routine.definition.name.clone(),
                        routine.definition.enabled,
                    )
                })
            })
            .collect()
    }

    /// The live drain, with its captured admission when it is grant-bound.
    fn explain_live_drain(&self) -> Result<Value, OrbitError> {
        let Some(run) = self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs("workspace_auto_pipeline")?
            .into_iter()
            .next()
        else {
            return Ok(Value::Null);
        };
        let admission = run
            .input
            .as_ref()
            .map(OperationAdmission::from_run_input)
            .transpose()
            .map_err(OrbitError::InvalidInput)?
            .flatten();
        let state = self.read_run_state(&run.run_id)?;
        Ok(json!({
            "run_id": run.run_id,
            "state": run.state.to_string(),
            "grant_id": admission.as_ref().map(|admission| admission.grant_id.clone()),
            "completion": admission.as_ref().map(|admission| admission.completion.clone()),
            "expires_at": admission.as_ref().map(|admission| admission.expires_at.to_rfc3339()),
            "admissions_stopped": state.as_ref().is_some_and(|state| state.admissions_stopped()),
            "worker_limit": state.and_then(|state| state.drain_worker_limit),
        }))
    }
}
