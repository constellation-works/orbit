//! Which machine owns a delivery consumer, and whether this host may admit.

use crate::OrbitRuntime;
use orbit_automation::{automation_error_to_orbit, delivery};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{DeliveryOwnership, DeliveryTrigger, OwnerAuthority};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, RoutineDefinition};

/// Resolve the effective owner of a delivery definition on this host.
///
/// An explicit `owner_machine` always wins. Otherwise this workspace's
/// registered owner answers, so enabling delivery automation on an
/// unambiguously owned workspace needs no redundant per-definition
/// configuration, while a replica of a workspace owned elsewhere cannot claim
/// it by omission. Ownership that is unregistered or self-contradictory
/// resolves to no owner at all: nothing is admitted, and the diagnostic says
/// which of the two it was.
pub(crate) fn resolve(runtime: &OrbitRuntime, explicit: Option<&str>) -> DeliveryOwnership {
    let (owner_machine, authority) = match explicit {
        Some(owner) => (Some(owner.to_string()), OwnerAuthority::Definition),
        None => registered_owner(runtime),
    };

    DeliveryOwnership {
        owned_here: owner_machine
            .as_deref()
            .is_some_and(|owner| Some(owner) == runtime.automation_machine_identity()),
        owner_machine,
        authority,
    }
}

/// Registry facts about the workspace this runtime is bound to. The workspace
/// record is the authority; a replica checkout's declared owner both covers a
/// legacy record that predates host identity and contradicts a record that
/// disagrees with this machine's role.
fn registered_owner(runtime: &OrbitRuntime) -> (Option<String>, OwnerAuthority) {
    match (
        runtime.workspace_owner_machine_id(),
        runtime.coordination_write_owner(),
    ) {
        (Some(workspace_owner), Some(replica_owner)) if workspace_owner != replica_owner => {
            (None, OwnerAuthority::Conflicting)
        }
        (Some(owner), _) | (None, Some(owner)) => {
            (Some(owner.to_string()), OwnerAuthority::Workspace)
        }
        (None, None) => (None, OwnerAuthority::Missing),
    }
}

/// The trigger every downstream decision uses: the definition's, with the
/// resolved owner substituted for whatever it declared.
///
/// The epoch is derived from it, so replacing an explicit owner with the
/// identical workspace default is not a definition change: the consumer keeps
/// its state, frozen batches, receipts and coverage. An owner that genuinely
/// differs still moves the epoch, as any other definition edit does.
pub(crate) fn with_resolved_owner(
    trigger: &DeliveryTrigger,
    ownership: &DeliveryOwnership,
) -> DeliveryTrigger {
    DeliveryTrigger {
        owner_machine: ownership.owner_machine.clone(),
        ..trigger.clone()
    }
}

/// Epoch of an auto-task definition under its resolved owner.
pub(crate) fn auto_task_epoch(
    definition: &AutoTaskDefinition,
    trigger: &DeliveryTrigger,
) -> Result<String, OrbitError> {
    delivery::definition_epoch(&(
        &AutoTaskSchedule::Deliveries {
            deliveries_landed: trigger.clone(),
        },
        &definition.template,
        definition.dedupe,
    ))
    .map_err(automation_error_to_orbit)
}

/// Epoch of a delivery routine under its resolved owner. The routine's retry
/// cap is applied after this, and has never been part of the epoch.
pub(crate) fn routine_epoch(
    definition: &RoutineDefinition,
    trigger: &DeliveryTrigger,
) -> Result<String, OrbitError> {
    let mut resolved = definition.trigger.clone();
    resolved.deliveries_landed = Some(trigger.clone());

    delivery::definition_epoch(&(&resolved, &definition.target, &definition.policy))
        .map_err(automation_error_to_orbit)
}
