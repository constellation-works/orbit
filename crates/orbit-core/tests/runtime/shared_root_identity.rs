//! Shared-root identity and coordination through the composed runtime boundary.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::adapter::HubCoordinationExecutor;
use orbit_core::{OrbitRuntime, ShipMode, WorkspaceRuntimeBinding};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{V2AuditEventFilter, V2AuditEventInsertParams};
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

fn binding(repo: &Path, logical: &str, partition: &str) -> WorkspaceRuntimeBinding {
    WorkspaceRuntimeBinding {
        logical_workspace_id: logical.into(),
        task_partition_id: partition.into(),
        owner_machine_id: None,
        checkout_role: None,
        repo_root: repo.into(),
        ship_mode: ShipMode::Local,
        base_branch: Some("main".into()),
    }
}

fn tool(runtime: &OrbitRuntime, name: &str, input: Value) -> Value {
    call(runtime, name, input).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn call(runtime: &OrbitRuntime, name: &str, input: Value) -> Result<Value, orbit_core::OrbitError> {
    runtime.run_tool_with_context_and_role(
        name,
        input,
        Role::Admin,
        ToolContext {
            model_name: Some("codex".into()),
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

fn reserve(runtime: &OrbitRuntime, ttl: u32) -> Value {
    tool(
        runtime,
        "orbit.task.locks.reserve",
        json!({"files": ["file:shared.txt"], "ttl_seconds": ttl}),
    )
}

fn claim(runtime: &OrbitRuntime, ttl: u32) -> Value {
    tool(
        runtime,
        "orbit.workspace.claim.acquire",
        json!({"ttl_seconds": ttl}),
    )
}

fn seed_recovery(runtime: &OrbitRuntime, event_id: &str, run_id: &str) {
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().unwrap(),
            event_id: event_id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "step_recovery_attempted".into(),
            ts: Utc::now(),
            run_id: run_id.into(),
            agent_identity: "codex".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({
                "event_id": event_id, "run_id": run_id,
                "body_kind": "step_recovery_attempted",
                "step_id": "failed-step", "recovery_activity": "recover",
                "recovery_succeeded": true,
            })
            .to_string(),
        })
        .unwrap();
}

#[test]
fn selected_shared_root_isolates_records_reservations_and_claims() {
    if !isolated(
        "shared_root_identity::selected_shared_root_isolates_records_reservations_and_claims",
    ) {
        return;
    }
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("shared-root");
    std::fs::create_dir_all(&root).unwrap();
    // The compatibility config belongs to the first registered repository.
    write_workspace_config(
        &root,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_alpha".into(),
        },
    )
    .unwrap();
    let open = |name: &str| {
        let repo = temp.path().join(name);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("shared.txt"), name).unwrap();
        let logical = format!("ws_{name}");
        HubCoordinationExecutor::register_workspace(&root, &logical, name).unwrap();
        OrbitRuntime::from_roots_with_binding(&root, &root, binding(&repo, &logical, "ws_alpha"))
            .unwrap()
    };
    let alpha = open("alpha");
    let beta = open("beta");
    assert_eq!(alpha.workspace_id().unwrap(), "ws_alpha");
    assert_eq!(beta.workspace_id().unwrap(), "ws_beta");
    // Opening the same root without a selected binding retains compatibility.
    assert_eq!(
        OrbitRuntime::from_roots(&root, &root)
            .unwrap()
            .workspace_id()
            .unwrap(),
        "ws_alpha"
    );

    let alpha_friction = tool(
        &alpha,
        "orbit.friction.add",
        json!({"body": "Alpha evidence"}),
    );
    let beta_friction = tool(
        &beta,
        "orbit.friction.add",
        json!({"body": "Beta evidence"}),
    );
    let alpha_only = tool(
        &alpha,
        "orbit.friction.add",
        json!({"body": "Alpha-only evidence"}),
    );
    for (runtime, own, expected_bodies) in [
        (
            &alpha,
            &alpha_friction,
            vec!["Alpha evidence", "Alpha-only evidence"],
        ),
        (&beta, &beta_friction, vec!["Beta evidence"]),
    ] {
        let records = tool(runtime, "orbit.friction.list", json!({}));
        let bodies = records
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["body"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(bodies, expected_bodies.into_iter().collect());
        assert_eq!(
            tool(runtime, "orbit.friction.show", json!({"id": own["id"]}))["body"],
            own["body"]
        );
    }
    // Friction IDs are local to each partition. Alpha's extra record has no
    // Beta counterpart, so looking it up through Beta must fail.
    let error = call(
        &beta,
        "orbit.friction.show",
        json!({"id": alpha_only["id"]}),
    )
    .unwrap_err();
    assert!(
        matches!(error, orbit_core::OrbitError::NotFound { .. }),
        "{error:?}"
    );

    seed_recovery(&alpha, "alpha-event", "alpha-run");
    seed_recovery(&beta, "beta-event", "beta-run");
    for (runtime, own, foreign, event_id) in [
        (&alpha, "alpha-run", "beta-run", "alpha-event"),
        (&beta, "beta-run", "alpha-run", "beta-event"),
    ] {
        let events = runtime
            .list_v2_audit_events(V2AuditEventFilter::default())
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, event_id);
        let recovery = runtime
            .collect_run_recovery_attempts_for_runs(&[own.into(), foreign.into()])
            .unwrap();
        assert_eq!(recovery.by_run_id[own].state, "recorded");
        assert_eq!(recovery.by_run_id[own].attempts[0].event_id, event_id);
        assert_eq!(recovery.by_run_id[foreign].state, "unavailable");
    }

    let alpha_lock = reserve(&alpha, 60);
    let beta_lock = reserve(&beta, 60);
    assert_eq!(alpha_lock["reserved"], true);
    assert_eq!(beta_lock["reserved"], true);
    assert_eq!(reserve(&alpha, 60)["reserved"], false);
    assert_eq!(reserve(&beta, 60)["reserved"], false);
    // A foreign release cannot remove another workspace's reservation.
    assert_eq!(
        tool(
            &beta,
            "orbit.task.locks.release",
            json!({"reservation_id": alpha_lock["reservation_id"]})
        )["released"],
        false
    );
    assert_eq!(
        tool(
            &alpha,
            "orbit.task.locks.release",
            json!({"reservation_id": alpha_lock["reservation_id"]})
        )["released"],
        true
    );
    assert_eq!(reserve(&alpha, 1)["reserved"], true);
    assert_eq!(reserve(&beta, 60)["reserved"], false);

    let alpha_claim = claim(&alpha, 60);
    let beta_claim = claim(&beta, 60);
    assert_eq!(alpha_claim["acquired"], true);
    assert_eq!(beta_claim["acquired"], true);
    assert_eq!(claim(&alpha, 60)["acquired"], false);
    assert_eq!(claim(&beta, 60)["acquired"], false);
    assert_eq!(
        tool(
            &beta,
            "orbit.workspace.claim.release",
            json!({"claim_token": alpha_claim["claim_token"]})
        )["released"],
        false
    );
    assert_eq!(
        tool(
            &alpha,
            "orbit.workspace.claim.release",
            json!({"claim_token": alpha_claim["claim_token"]})
        )["released"],
        true
    );
    assert_eq!(claim(&alpha, 1)["acquired"], true);

    // Expiry frees only Alpha; Beta's longer leases still enforce conflicts.
    let deadline = Instant::now() + Duration::from_secs(5);
    while tool(&alpha, "orbit.task.locks", json!({}))["total_reservations"] != 0 {
        assert!(Instant::now() < deadline, "Alpha's reservation must expire");
        std::thread::sleep(Duration::from_millis(50));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if claim(&alpha, 60)["acquired"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "Alpha's claim must expire");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(reserve(&alpha, 60)["reserved"], true);
    assert_eq!(reserve(&beta, 60)["reserved"], false);
    assert_eq!(claim(&beta, 60)["acquired"], false);
}

#[test]
fn ordinary_checkout_keeps_its_partition_after_logical_registration() {
    if !isolated(
        "shared_root_identity::ordinary_checkout_keeps_its_partition_after_logical_registration",
    ) {
        return;
    }
    let temp = TempDir::new().unwrap();
    let global = temp.path().join("global");
    let repo = temp.path().join("repo");
    let root = repo.join(".orbit");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(repo.join("shared.txt"), "fixture").unwrap();
    let unregistered = OrbitRuntime::from_roots(&global, &root).unwrap();
    let partition = unregistered.workspace_id().unwrap();
    let friction = tool(
        &unregistered,
        "orbit.friction.add",
        json!({"body": "Existing checkout evidence"}),
    );
    seed_recovery(&unregistered, "checkout-event", "checkout-run");
    assert_eq!(reserve(&unregistered, 60)["reserved"], true);
    assert_eq!(claim(&unregistered, 60)["acquired"], true);

    let registered = OrbitRuntime::from_roots_with_binding(
        &global,
        &root,
        binding(&repo, "ws_registered", &partition),
    )
    .unwrap();
    assert_ne!(partition, "ws_registered");
    assert_eq!(registered.workspace_id().unwrap(), partition);
    assert_eq!(
        tool(&registered, "orbit.friction.list", json!({}))[0]["id"],
        friction["id"]
    );
    assert_eq!(
        registered
            .list_v2_audit_events(V2AuditEventFilter {
                source: Some("v2_envelope".into()),
                ..Default::default()
            })
            .unwrap()[0]
            .event_id,
        "checkout-event"
    );
    assert_eq!(
        registered
            .collect_run_recovery_attempts("checkout-run")
            .unwrap()
            .state,
        "recorded"
    );
    assert_eq!(reserve(&registered, 60)["reserved"], false);
    assert_eq!(claim(&registered, 60)["acquired"], false);
}
