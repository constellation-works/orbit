//! The shared admission decision every retained drain entry point takes.

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, ExecutionLocation,
};

use super::contract::owner_binary_version;
use crate::runtime::host_signal::{HOST_SHUTDOWN_SCHEDULED, ScheduledShutdown};

/// A retained entry point that admits workspace delivery work [ORB-12500].
///
/// Every one of these existed before the distributed drain and keeps its own
/// surface, schedule and enablement. What they no longer keep is a private
/// idea of what the host is already doing: they all ask
/// [`OrbitRuntime::drain_entry_admission`], which reads one occupancy, one
/// claim ledger and one destination-authority rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainEntryPoint {
    /// `orbit run auto`, and the seeded `ship_sweep` routine and
    /// `workspace_ship_pipeline` wrapper that invoke the drain beneath it.
    OwnerDrain,
    /// `orbit run ship`, `orbit.workflow.ship`, and the dashboard endpoint —
    /// the explicit shipment surfaces, with or without named tasks.
    ExplicitShip,
    /// The independent registry-driven `orbit run ship-sweep` CLI, which
    /// dispatches per workspace without a workspace runtime of its own.
    ShipSweep,
}

impl DrainEntryPoint {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DrainEntryPoint::OwnerDrain => "orbit.workflow.auto",
            DrainEntryPoint::ExplicitShip => "orbit.workflow.ship",
            DrainEntryPoint::ShipSweep => "orbit.run.ship-sweep",
        }
    }
}

/// Why a retained entry point may not admit right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainEntryRefusal {
    /// This checkout is a replica. Owner coordination work — backlog
    /// selection, reservation, claim settlement — belongs to the owner, and a
    /// replica executes through pull instead.
    Replica { owner_machine_id: String },
    /// Every local drain slot is taken, counting legacy wrappers, claimed
    /// leaves and pending admissions no run represents yet, from the one
    /// reading the pull allocator commits against.
    Saturated { occupied: usize },
    /// A live claim already protects this task's frozen footprint. The
    /// remedy is the claim's own lifecycle — settlement or deliberate
    /// recovery — never a second admission.
    Claimed {
        task_id: String,
        claim_id: String,
        machine_id: String,
    },
    /// The host has a shutdown or reboot pending that would kill anything
    /// started now [ORB-12968]. Only unattended entry points stand down for
    /// it; the hold lifts on its own when the schedule is cancelled or the
    /// host has restarted.
    HostShutdownScheduled { shutdown: ScheduledShutdown },
}

impl DrainEntryRefusal {
    /// A stable code a scheduler can branch on, matching the shapes the sweep
    /// already reports.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            DrainEntryRefusal::Replica { .. } => "replica_checkout",
            DrainEntryRefusal::Saturated { .. } => "ship_in_flight",
            DrainEntryRefusal::Claimed { .. } => "claimed_by_execution_claim",
            DrainEntryRefusal::HostShutdownScheduled { .. } => HOST_SHUTDOWN_SCHEDULED,
        }
    }

    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            DrainEntryRefusal::Replica { owner_machine_id } => format!(
                "this checkout is a replica of machine '{owner_machine_id}'; owner-only \
                 coordination work is refused here and a replica executes through pull. Start \
                 its drain with `orbit run auto --pull <selector>` (the owner's selector from \
                 federated orbit.workspace.list), or run this on the owner's checkout"
            ),
            DrainEntryRefusal::Saturated { occupied } => format!(
                "{occupied} drain slot(s) are already occupied by live leaves or pending \
                 admissions; this entry point stands down rather than admitting beside them"
            ),
            DrainEntryRefusal::Claimed {
                task_id,
                claim_id,
                machine_id,
            } => format!(
                "task '{task_id}' is held by execution claim '{claim_id}' on machine \
                 '{machine_id}'; it settles or is deliberately recovered, never admitted twice"
            ),
            DrainEntryRefusal::HostShutdownScheduled { shutdown } => shutdown.hold_reason(),
        }
    }
}

/// One shared admission decision, whatever surface asked for it.
#[derive(Debug, Clone)]
pub struct DrainEntryAdmission {
    pub entry_point: DrainEntryPoint,
    /// The single capacity reading legacy dispatch and pull both allocate
    /// against.
    pub occupancy: orbit_store::contracts::DrainLeafOccupancy,
    /// The owner's effective review policy. v1 admits only `none` through the
    /// claim contract, which is why it is reported on every decision rather
    /// than left for each surface to look up.
    pub review_policy: String,
    /// Whether the claim contract would admit this workspace at all — the
    /// same ordered ladder `orbit.task.pull` applies, so a preflight and a
    /// retained entry cannot disagree about it.
    pub claim_admission_refusal: Option<String>,
    /// The host shutdown pending when the decision was taken. An unattended
    /// entry point is refused for it; an explicit one is admitted with a
    /// warning, and the drain it starts still holds its own waves.
    pub host_shutdown: Option<ScheduledShutdown>,
    pub refusal: Option<DrainEntryRefusal>,
}

impl DrainEntryAdmission {
    /// Turn a refusal into an error, for a surface that has no "stood down"
    /// outcome of its own.
    pub fn into_result(self) -> Result<Self, OrbitError> {
        match &self.refusal {
            None => Ok(self),
            Some(DrainEntryRefusal::Replica { .. }) => {
                Err(OrbitError::CapabilityRefused(self.refusal_reason()))
            }
            Some(_) => Err(OrbitError::PolicyDenied(self.refusal_reason())),
        }
    }

    fn refusal_reason(&self) -> String {
        self.refusal
            .as_ref()
            .map(DrainEntryRefusal::reason)
            .unwrap_or_default()
    }
}

impl crate::OrbitRuntime {
    /// The shared admission decision every retained entry point makes.
    ///
    /// `unattended` is what separates a sweep from an operator's explicit
    /// invocation: a sweep that finds the host busy, or finds a host shutdown
    /// scheduled, skips that workspace, while `orbit run ship` is a deliberate
    /// act whose own leaf definition already bounds it and which proceeds
    /// past a scheduled shutdown with a warning. Neither may bypass the claim
    /// ledger or serve owner coordination from a replica.
    pub fn drain_entry_admission(
        &self,
        entry_point: DrainEntryPoint,
        task_ids: &[String],
        unattended: bool,
    ) -> Result<DrainEntryAdmission, OrbitError> {
        let ship = self.owner_ship_contract();
        let occupancy = self.stores().jobs().drain_leaf_occupancy()?;
        let mut decision = DrainEntryAdmission {
            entry_point,
            occupancy,
            review_policy: ship.review_policy.clone(),
            claim_admission_refusal: self.claim_contract_refusal(&ship),
            host_shutdown: self.scheduled_host_shutdown(),
            refusal: None,
        };
        if let Some(owner_machine_id) = self.coordination_write_owner() {
            decision.refusal = Some(DrainEntryRefusal::Replica {
                owner_machine_id: owner_machine_id.to_string(),
            });
            return Ok(decision);
        }
        // The claim ledger is consulted before capacity: "this exact task is
        // already being executed" is the more actionable of the two, and a
        // saturated host would otherwise mask it.
        if !task_ids.is_empty() {
            let claims = self.inspect_execution_claims()?;
            if let Some(claim) = claims
                .iter()
                .map(|inspection| &inspection.claim)
                .find(|claim| {
                    claim.phase.protects_footprint()
                        && task_ids.iter().any(|task_id| task_id == &claim.task_id)
                })
            {
                decision.refusal = Some(DrainEntryRefusal::Claimed {
                    task_id: claim.task_id.clone(),
                    claim_id: claim.claim_id.clone(),
                    machine_id: claim.executed_on.machine_id.clone(),
                });
                return Ok(decision);
            }
        }
        if let Some(shutdown) = decision.host_shutdown.as_ref() {
            if unattended {
                decision.refusal = Some(DrainEntryRefusal::HostShutdownScheduled {
                    shutdown: shutdown.clone(),
                });
                return Ok(decision);
            }
            tracing::warn!(
                target: "orbit.core.host_signal",
                entry_point = entry_point.label(),
                mode = shutdown.mode.as_str(),
                scheduled_at = %shutdown.scheduled_at,
                "explicit admission proceeds although {}; the run may be killed by it",
                shutdown.describe(),
            );
        }
        if unattended && decision.occupancy.occupied > 0 {
            decision.refusal = Some(DrainEntryRefusal::Saturated {
                occupied: decision.occupancy.occupied,
            });
        }
        Ok(decision)
    }

    /// Whether the claim contract would admit this workspace, by the spec's
    /// own ordered ladder. Reported rather than raised: a workspace whose
    /// review policy is not `none` still ships through its legacy leaf, and
    /// saying so is what keeps the two facts from being confused.
    fn claim_contract_refusal(&self, ship: &AdmissionShipContract) -> Option<String> {
        let identity = AdmissionIdentity::trusted_local(ExecutionLocation {
            machine_id: self
                .automation_machine_identity()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "local".to_string()),
            machine_name: None,
        });
        let request = AdmissionRequest {
            request_id: "entry-point".to_string(),
            caller_version: owner_binary_version().to_string(),
            caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_review_policy: ship.review_policy.clone(),
            run_context: AdmissionRunContext {
                run_id: "entry-point".to_string(),
                job_name: "entry-point".to_string(),
                machine_name: None,
            },
            ship: ship.clone(),
            crews: None,
        };
        orbit_store::admission_refusal(&identity, &request, owner_binary_version())
            .map(|refusal| refusal.as_str().to_string())
    }
}
