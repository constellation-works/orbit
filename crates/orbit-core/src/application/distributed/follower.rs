//! Submitting a follower's pull drain: `orbit run auto --pull <selector>`
//! [ORB-13625].
//!
//! Everything a pull drain will trust is established here, before a run
//! exists, from facts this host and the owner report — never from the
//! operator's word alone:
//!
//! - this checkout is a **replica**, and the selector names **its** owner
//!   machine and **its** logical workspace;
//! - the owner answers the read-only probe **as that machine**, and would
//!   admit this executor now (binary, protocol schema, review policy, ship
//!   mode);
//! - this host declares the required validation commands a claimed leaf must
//!   pass, because an empty list fails every handoff closed.
//!
//! The resolved destination is persisted on the run, so every iteration, leaf
//! and retry addresses the same owner and workspace. A destination that later
//! stops answering is reported by the drain, never replaced by a local store.

use orbit_common::OrbitError;
use orbit_store::contracts::{DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, PullDestination};
use orbit_types::workflow::{DrainAdmissionPass, JobRunTrigger, ResourceThrottle};
use serde_json::{Value, json};

use super::{ensure_distributed_mutation_available, owner_binary_version};
use crate::application::job::PipelineInvokeResult;

/// The job a follower drain runs.
pub const PULL_DRAIN_JOB: &str = "workspace_pull_pipeline";

const PASS_FAILURE_THRESHOLD: u32 = 3;

/// Operator inputs for one follower drain.
#[derive(Debug, Clone, Default)]
pub struct WorkspacePullRequest<'a> {
    /// Host-qualified owner selector, copied from federated discovery.
    pub selector: &'a str,
    pub for_seconds: Option<u64>,
    pub max_active_leaf_runs: Option<u32>,
    pub actor: Option<&'a str>,
}

impl crate::OrbitRuntime {
    /// Persist pass health atomically with the existing admission observation.
    /// Degradation latches for the window, so a successful settlement-only pass
    /// cannot hide the failure that stopped admissions.
    pub(crate) fn record_pull_pass(
        &self,
        run_id: &str,
        resource_throttle: Option<ResourceThrottle>,
        error: Option<&str>,
    ) -> Result<DrainAdmissionPass, OrbitError> {
        let mut recorded = None;
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                let previous = state.drain_last_pass.as_ref();
                let was_degraded = previous.is_some_and(|pass| pass.degraded);
                let previous_count = previous.map_or(0, |pass| pass.consecutive_pass_failures);
                let consecutive_pass_failures = if error.is_some() {
                    previous_count.saturating_add(1)
                } else if was_degraded {
                    previous_count
                } else {
                    0
                };
                let pass = DrainAdmissionPass {
                    recorded_at: chrono::Utc::now(),
                    queued: 0,
                    deferred: Vec::new(),
                    excluded: Vec::new(),
                    excluded_total: 0,
                    resource_throttle: resource_throttle.clone(),
                    last_pass_error: error.map(ToOwned::to_owned).or_else(|| {
                        was_degraded
                            .then(|| previous.and_then(|pass| pass.last_pass_error.clone()))
                            .flatten()
                    }),
                    consecutive_pass_failures,
                    degraded: was_degraded || consecutive_pass_failures >= PASS_FAILURE_THRESHOLD,
                };
                state.drain_last_pass = Some(pass.clone());
                recorded = Some(pass);
                Ok(())
            })?;
        recorded.ok_or_else(|| OrbitError::Store("pull drain pass was not recorded".into()))
    }

    /// Verify this replica against its owner and submit its pull drain.
    pub fn submit_workspace_pull_run(
        &self,
        request: WorkspacePullRequest<'_>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.workflow.auto --pull")?;
        // Operator input that needs no owner is checked before the probe, so a
        // typo costs no network round trip.
        if request.max_active_leaf_runs == Some(0) {
            return Err(OrbitError::InvalidInput(
                "--concurrency must be at least 1".into(),
            ));
        }
        let destination = self.resolve_pull_destination(request.selector)?;
        if self.workflow_required_validation_commands().is_empty() {
            return Err(OrbitError::InvalidInput(
                "this host declares no `workflow.required_validation_commands`; a claimed leaf \
                 must run the owner's required validation, and an empty list fails every \
                 handoff closed. Set the same list the owner uses (`orbit config get \
                 workflow.required_validation_commands` there, then `orbit config set \
                 workflow.required_validation_commands '<list>'` here)."
                    .into(),
            ));
        }
        let mut input = json!({
            "for_seconds": request.for_seconds.unwrap_or(0),
            "destination": serde_json::to_value(&destination)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
        });
        if let Some(ceiling) = request.max_active_leaf_runs {
            input["max_active_leaf_runs"] = json!(ceiling);
        }
        self.submit_pipeline_run_with_trigger(PULL_DRAIN_JOB, input, None, request.actor, trigger)
    }

    /// The owner this replica pulls from, checked against the replica's own
    /// registration and the owner's live answer.
    fn resolve_pull_destination(&self, selector: &str) -> Result<PullDestination, OrbitError> {
        let selector = selector.trim();
        let owner_machine = self.coordination_write_owner().ok_or_else(|| {
            OrbitError::CapabilityRefused(
                "`--pull` runs on a replica checkout; this checkout is its workspace's owner, \
                 whose drain is plain `orbit run auto`"
                    .into(),
            )
        })?;
        let (selector_machine, selector_workspace) = selector.split_once('/').ok_or_else(|| {
            OrbitError::UnknownSelector(format!(
                "'{selector}' is not a host-qualified selector; copy `selector` from federated \
                 orbit.workspace.list"
            ))
        })?;
        if selector_machine != owner_machine {
            return Err(OrbitError::InvalidInput(format!(
                "selector names machine '{selector_machine}', but this replica's owner is \
                 '{owner_machine}'"
            )));
        }
        let logical = self
            .workspace_runtime_binding()
            .map(|binding| binding.logical_workspace_id.as_str())
            .ok_or_else(|| {
                OrbitError::InvalidInput("this checkout is not a registered workspace".into())
            })?;
        if selector_workspace != logical {
            return Err(OrbitError::InvalidInput(format!(
                "selector names workspace '{selector_workspace}', but this replica is registered \
                 as '{logical}'"
            )));
        }
        let execution_machine = self
            .automation_machine_identity()
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "this host has no registered machine identity; run `orbit init` first".into(),
                )
            })?
            .to_string();
        let transport = self.drain_owner_transport().ok_or_else(|| {
            OrbitError::InvalidInput(
                "this runtime has no federated owner route; add the owner to \
                 ~/.orbit/mcp-destinations.toml"
                    .into(),
            )
        })?;
        let report = transport
            .call(
                selector,
                "orbit.drain.probe",
                json!({
                    "caller_version": owner_binary_version(),
                    "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                    "caller_review_policy": self.local_review_policy_label(),
                }),
            )
            .map_err(|error| match error {
                // The selector parsed and names this replica's own owner, so
                // the only thing missing is a route to that machine.
                OrbitError::UnknownSelector(token) => OrbitError::UnknownSelector(format!(
                    "{token}: this host has no destination for owner machine '{owner_machine}'; \
                     add it to ~/.orbit/mcp-destinations.toml and check the selector against \
                     federated orbit.workspace.list"
                )),
                other => other,
            })?;
        let answered_as = report.get("owner_machine_id").and_then(Value::as_str);
        if answered_as != Some(owner_machine) {
            return Err(OrbitError::InvalidInput(format!(
                "the owner answered as {answered_as:?}, not this replica's owner '{owner_machine}'"
            )));
        }
        if let Some(refusal) = super::protocol_mismatch(&report) {
            return Err(OrbitError::CapabilityRefused(refusal));
        }
        if report.get("admits").and_then(Value::as_bool) != Some(true) {
            let diagnostics = report
                .get("diagnostics")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            return Err(OrbitError::CapabilityRefused(format!(
                "the owner would refuse this executor ({}): {diagnostics}",
                report
                    .get("refusal")
                    .and_then(Value::as_str)
                    .unwrap_or("refused")
            )));
        }
        let owner_workspace_id = report
            .get("workspace_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| OrbitError::Store("owner probe reported no workspace id".into()))?
            .to_string();
        Ok(PullDestination {
            owner_machine_id: owner_machine.to_string(),
            owner_workspace_id,
            selector: selector.to_string(),
            execution_machine_id: execution_machine,
        })
    }
}
