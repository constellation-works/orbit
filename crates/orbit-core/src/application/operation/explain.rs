//! The effective-policy explanation: current or preview preferences, the
//! authority actually in force, and each cap or limiting reason that reduces
//! requested behavior [ORB-11332].
//!
//! Preferences (`[operation]`, plus an optional run-layer preview) describe
//! what a *future* grant would capture. When an active grant exists, live
//! delivery, preparation, recovery, review, caps, and limiting reasons are
//! projected from that grant's captured policy, not from the current file.
//! This is a read-only projection; it never schedules, promotes, or
//! authorizes anything.

use chrono::Utc;
use orbit_automation::routines::loader::{RoutineSource, collect_routines};
use orbit_common::OrbitError;
use orbit_config::{OperationLayer, PreparationPreference, RecoveryPreference, ReviewPolicy};
use orbit_types::workflow::OperationAdmission;
use orbit_types::workflow::automation::members::StateTriggerKind;
use serde_json::{Value, json};

use super::{NO_GRANT_REASON, captured_policy};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Explain operation preferences and live authority, optionally with
    /// run-layer overrides applied as a preview of a future grant.
    pub fn explain_operation(
        &self,
        run_layer: Option<&OperationLayer>,
    ) -> Result<Value, OrbitError> {
        let preferences = match run_layer {
            Some(layer) => self.operation_policy().with_run_layer(layer),
            None => self.operation_policy().clone(),
        };
        let now = Utc::now();
        let hard_limit = self.leaf_run_hard_limit()?;
        let grant = self.active_operation_grant()?;
        let active_policy = match &grant {
            Some(grant) => captured_policy(grant)?,
            None => preferences.clone(),
        };
        let mut limiting_reasons = Vec::new();

        let (effective_completion, cap_reason) = active_policy.capped_completion();
        if let Some(reason) = cap_reason {
            limiting_reasons.push(reason.to_string());
        }
        let leaf_cap = (active_policy.leaf_ceiling.value > hard_limit).then_some("job_hard_limit");
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
                    "policy": active_policy.explain(),
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
        let preparation_reason = if active_policy.preparation.value
            == PreparationPreference::Automatic
            && !preparation_owners
                .iter()
                .any(|owner| owner["enabled"] == true)
        {
            limiting_reasons.push("no_enabled_preparation_routine".to_string());
            Some("no_enabled_preparation_routine")
        } else {
            None
        };
        let recovery_reason = if active_policy.recovery.value == RecoveryPreference::Scheduled
            && !triage_owners.iter().any(|owner| owner["enabled"] == true)
        {
            limiting_reasons.push("no_enabled_triage_routine".to_string());
            Some("no_enabled_triage_routine")
        } else {
            None
        };
        // [ORB-11333] A before-PR gate needs an explicitly configured
        // reviewer crew; without one every gated delivery escalates.
        let review_reason = (active_policy.review_policy.value == ReviewPolicy::BeforePr
            && active_policy.review_crew.value.is_none())
        .then(|| {
            limiting_reasons.push("review_crew_unconfigured".to_string());
            "review_crew_unconfigured"
        });

        let drain = self.explain_live_drain()?;

        Ok(json!({
            "preview": run_layer.is_some(),
            "policy": preferences.explain(),
            "authority": authority,
            "limits": {
                "leaf_ceiling": {
                    "preference": active_policy.leaf_ceiling.value,
                    "hard_limit": hard_limit,
                    "effective": active_policy.leaf_ceiling.value.min(hard_limit),
                    "cap": leaf_cap,
                },
                "window_max_seconds": orbit_types::workflow::MAX_GRANT_WINDOW_SECONDS,
                "scope_max_tasks": orbit_types::workflow::MAX_GRANT_SCOPE_TASKS,
            },
            "delivery": {
                "completion_preference": active_policy.completion.value,
                "delivery_cap": active_policy.delivery_cap.value,
                "effective_completion": effective_completion,
                "cap": cap_reason,
                "grant_complete_right": grant.as_ref().map(|grant| grant.rights.complete),
            },
            "preparation": {
                "preference": active_policy.preparation.value,
                "due_seconds": active_policy.preparation_due_seconds.value,
                "cadence_owners": preparation_owners,
                "reason": preparation_reason,
            },
            "recovery": {
                "preference": active_policy.recovery.value,
                "episodes_per_task": active_policy.recovery_episodes_per_task.value,
                "minutes_per_task": active_policy.recovery_minutes_per_task.value,
                "triage_owners": triage_owners,
                "reason": recovery_reason,
            },
            "review": {
                "policy": active_policy.review_policy.value,
                "policy_source": active_policy.review_policy.source.label(),
                "gates_pr": active_policy.review_policy.value == ReviewPolicy::BeforePr,
                "crew": active_policy.review_crew.value,
                "crew_source": active_policy.review_crew.source.label(),
                "budget": active_policy.review_budget(),
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
