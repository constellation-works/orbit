//! Sibling tests for `base/role.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::task::ExecutionLocation;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::ShipMode;
use orbit_types::workspace::WorkspaceCheckoutRole;

use super::super::binding::WorkspaceRuntimeBinding;
use super::super::role::Role;

fn binding(
    checkout_role: Option<WorkspaceCheckoutRole>,
    owner_machine_id: Option<&str>,
) -> WorkspaceRuntimeBinding {
    WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_test".to_string(),
        task_partition_id: "ws_test".to_string(),
        owner_machine_id: owner_machine_id.map(str::to_string),
        checkout_role,
        repo_root: PathBuf::from("/tmp/repo"),
        ship_mode: ShipMode::Local,
        base_branch: None,
    }
}

fn worker_invocation() -> Arc<WorkerInvocation> {
    Arc::new(WorkerInvocation {
        owner_machine_id: "hm_owner".to_string(),
        owner_workspace_id: "ws_test".to_string(),
        owner_destination: "hm_owner/ws_test".to_string(),
        task_id: "ORB-1".to_string(),
        claim_id: "clm_1".to_string(),
        execution: ExecutionLocation {
            machine_id: "hm_worker".to_string(),
            machine_name: Some("worker".to_string()),
        },
        bound_run_id: "jrun-1".to_string(),
    })
}

#[test]
fn resolves_each_process_role_and_its_accessor_answers() {
    // 1. Operator command: no binding, unbound checkout, or replica binding lacking an owner.
    let operator = Role::resolve(None, None);
    assert_eq!(operator, Role::Operator);
    assert_eq!(operator.invocation(), None);
    assert_eq!(operator.replica_owner(), None);
    assert!(operator.task_reads_visible());
    assert!(operator.task_write_refusal().is_none());

    let unbound = binding(None, None);
    assert_eq!(Role::resolve(Some(&unbound), None), Role::Operator);

    let ownerless_replica = binding(Some(WorkspaceCheckoutRole::Replica), None);
    assert_eq!(
        Role::resolve(Some(&ownerless_replica), None),
        Role::Operator
    );

    // 2. Drain owner: registered owner checkout with no worker invocation.
    let owner_binding = binding(Some(WorkspaceCheckoutRole::Owner), Some("hm_owner"));
    let drain_owner = Role::resolve(Some(&owner_binding), None);
    assert_eq!(drain_owner, Role::DrainOwner);
    assert_eq!(drain_owner.invocation(), None);
    assert_eq!(drain_owner.replica_owner(), None);
    assert!(drain_owner.task_reads_visible());
    assert!(drain_owner.task_write_refusal().is_none());

    // 3. Worker: carries its invocation regardless of checkout binding, and keeps it across
    // replica-owner overrides.
    let invocation = worker_invocation();
    let replica_binding = binding(Some(WorkspaceCheckoutRole::Replica), Some("hm_owner"));
    let worker = Role::resolve(Some(&replica_binding), Some(Arc::clone(&invocation)));
    assert_eq!(worker, Role::Worker(Arc::clone(&invocation)));
    assert_eq!(worker.invocation(), Some(invocation.as_ref()));
    assert_eq!(worker.replica_owner(), None);
    assert!(worker.task_reads_visible());
    assert!(matches!(
        worker.task_write_refusal(),
        Some(OrbitError::PolicyDenied(ref msg))
            if msg == "claimed coordination writes require the owner route"
    ));
    assert_eq!(
        worker.with_replica_owner_override(Some(&replica_binding), Some("hm_owner".to_string())),
        worker
    );
    assert_eq!(
        worker.with_replica_owner_override(Some(&owner_binding), None),
        worker
    );

    // 4. Replica checkout: carries the owning machine id, hides local task reads, and refuses
    // control-plane writes with CapabilityRefused naming the owner.
    let replica = Role::resolve(Some(&replica_binding), None);
    assert_eq!(
        replica,
        Role::Replica {
            owner_machine_id: Arc::from("hm_owner"),
        }
    );
    assert_eq!(replica.invocation(), None);
    assert_eq!(replica.replica_owner(), Some("hm_owner"));
    assert!(!replica.task_reads_visible());
    assert!(matches!(
        replica.task_write_refusal(),
        Some(OrbitError::CapabilityRefused(ref msg))
            if msg == "control_plane coordination writes are refused in this replica checkout; workspace is owned by machine 'hm_owner'"
    ));

    // Explicit replica-owner overrides route through the same resolver.
    let overridden_replica =
        operator.with_replica_owner_override(None, Some("hm_remote".to_string()));
    assert_eq!(overridden_replica.replica_owner(), Some("hm_remote"));
    assert!(!overridden_replica.task_reads_visible());
    assert_eq!(
        overridden_replica.with_replica_owner_override(None, None),
        Role::Operator
    );
    assert_eq!(
        drain_owner.with_replica_owner_override(Some(&owner_binding), None),
        Role::DrainOwner
    );
}
