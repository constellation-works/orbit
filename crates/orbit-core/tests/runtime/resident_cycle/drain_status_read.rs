//! A live `resident_cycle_pipeline` run reads drain status (`orbit.workflow.auto`
//! action `status`) for its covered workspaces with or without any granted
//! right; an uncovered workspace is refused as `resident_scope_refused`; drain
//! controls and callers outside a live resident cycle keep the operator
//! requirement (ORB-26832).

use std::sync::{Arc, OnceLock};

use orbit_common::OrbitError;
use orbit_core::{FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope};
use orbit_types::task::TaskArtifact;

use super::*;

struct HostWorkspaces {
    targets: Vec<FederatedWorkspaceTarget>,
    runtimes: OnceLock<Vec<OrbitRuntime>>,
}

impl WorkspaceCatalog for HostWorkspaces {
    fn resolve_scope(
        &self,
        scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError> {
        Ok(match scope {
            WorkspaceScope::Selectors(selectors) => self
                .targets
                .iter()
                .filter(|target| {
                    selectors.iter().any(|selector| {
                        selector == &target.workspace_id || selector == &target.name
                    })
                })
                .cloned()
                .collect(),
            _ => self.targets.clone(),
        })
    }

    fn open(
        &self,
        target: &FederatedWorkspaceTarget,
    ) -> Result<orbit_runtime::RuntimeKernel, OrbitError> {
        let index = self
            .targets
            .iter()
            .position(|known| known.workspace_id == target.workspace_id)
            .expect("the catalog opens only workspaces it resolved");
        Ok(self.runtimes.get().unwrap()[index].clone().into_kernel())
    }
}

/// Attach a second registered workspace sharing the host's `global` store.
fn add_host_workspace(fx: &mut Fixture) -> (String, OrbitRuntime, TempDir) {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let other_repo = root.path().join("other");
    std::fs::create_dir_all(other_repo.join(".orbit")).unwrap();
    std::fs::write(other_repo.join(".orbit/config.toml"), CONFIG).unwrap();
    let other_raw = OrbitRuntime::from_roots(&fx.global, &other_repo.join(".orbit")).unwrap();
    let home_id = fx.runtime.workspace_id().unwrap();
    let other_id = other_raw.workspace_id().unwrap();
    assert_ne!(home_id, other_id);
    let catalog = Arc::new(HostWorkspaces {
        targets: vec![
            FederatedWorkspaceTarget {
                workspace_id: home_id,
                name: fx.runtime.workspace_label(),
                repo_root: fx.repo.clone(),
            },
            FederatedWorkspaceTarget {
                workspace_id: other_id.clone(),
                name: "other".into(),
                repo_root: other_repo,
            },
        ],
        runtimes: OnceLock::new(),
    });
    let home = fx.runtime.clone().with_workspace_catalog(catalog.clone());
    let other = other_raw.with_workspace_catalog(catalog.clone());
    let _ = catalog.runtimes.set(vec![home.clone(), other.clone()]);
    fx.runtime = home;
    (other_id, other, root)
}

fn drain_status(
    runtime: &OrbitRuntime,
    workspace: &str,
    capability: McpCapability,
) -> Result<Value, String> {
    call(
        runtime,
        "orbit.workflow",
        json!({"action": "auto", "mode": "status", "workspace": workspace}),
        capability,
    )
}

fn assert_status_payload(reply: &Value, workspace: &str, expected_controls_authorized: bool) {
    assert_eq!(reply["schema_version"], 1, "{reply}");
    assert_eq!(reply["scope"], "drain", "{reply}");
    assert_eq!(reply["workspace"], workspace, "{reply}");
    assert_eq!(
        reply["controls_authorized"], expected_controls_authorized,
        "{reply}"
    );
    assert!(reply["capacity"].is_object(), "{reply}");
    assert!(reply["tasks"].is_array(), "{reply}");
}

#[test]
fn a_resident_cycle_reads_drain_status_for_covered_workspaces_without_any_right() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::drain_status_read::a_resident_cycle_reads_drain_status_for_covered_workspaces_without_any_right",
    ) {
        return;
    }
    let mut fx = fixture();
    let (other_id, other_runtime, _keep_other) = add_host_workspace(&mut fx);
    let home_id = fx.runtime.workspace_id().unwrap();
    let watched = fx.task("Watched");

    // Mandate with NO rights granted at all (never called `orbit resident grant`).
    let no_grant_mandate = fx.mandate(&watched);
    let run_no_grant = fx.cycle_run(&no_grant_mandate, "resident-drain-status-no-grant");
    let brief = fx.begin_cycle(&run_no_grant, Utc::now());
    assert_eq!(brief["cycle_count"], 1, "{brief}");

    fx.as_cycle_agent(&run_no_grant, || {
        let reply = drain_status(&fx.runtime, &home_id, McpCapability::Agent)
            .expect("a cycle with no granted rights still reads its own workspace drain status");
        assert_status_payload(&reply, &home_id, false);
    });
    fx.runtime.finish_resident_cycle(&run_no_grant).unwrap();

    // Mandate covering both `home` and `other`, with `workspaces` granted and
    // zero rights (`add: []`).
    let multi_mandate = fx.add_task(TaskAddParams {
        title: "[orchestration]: multi-ws - sol".into(),
        tags: vec!["resident".into()],
        ..Default::default()
    });
    let mandate_body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Coordinate across covered workspaces",
        "exit_criteria": ["All covered work completes"],
        "workspaces": [home_id, other_id],
        "watch": {"tasks": [watched], "tags": []},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    fx.update_task(
        &multi_mandate,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "resident/mandate.json",
                mandate_body.to_string(),
            )],
            ..Default::default()
        },
    );
    fx.operator_call(
        "orbit.resident.grant",
        json!({"task_id": multi_mandate, "workspaces": [home_id, other_id]}),
    );
    let run_multi = fx.cycle_run(&multi_mandate, "resident-drain-status-multi-ws");
    let brief = fx.begin_cycle(&run_multi, Utc::now());
    assert_eq!(brief["cycle_count"], 1, "{brief}");

    fx.as_cycle_agent(&run_multi, || {
        let home_reply = drain_status(&fx.runtime, &home_id, McpCapability::Agent)
            .expect("reads home workspace drain status without rights");
        assert_status_payload(&home_reply, &home_id, false);

        // Routed to the second covered workspace's runtime, as CLI/MCP does.
        let other_reply = drain_status(&other_runtime, &other_id, McpCapability::Agent)
            .expect("reads covered workspace drain status without rights");
        assert_status_payload(&other_reply, &other_id, false);
    });
    fx.runtime.finish_resident_cycle(&run_multi).unwrap();

    // Also succeeds when the cycle holds a right (e.g. `dispatch`).
    let dispatch_mandate = fx.mandate(&watched);
    fx.grant(&dispatch_mandate, &["dispatch"]);
    let run_with_right = fx.cycle_run(&dispatch_mandate, "resident-drain-status-with-right");
    let brief = fx.begin_cycle(&run_with_right, Utc::now());
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    fx.as_cycle_agent(&run_with_right, || {
        let reply = drain_status(&fx.runtime, &home_id, McpCapability::Agent)
            .expect("reads drain status with dispatch right too");
        assert_status_payload(&reply, &home_id, false);
    });
}

#[test]
fn drain_status_for_uncovered_workspace_is_refused_as_resident_scope_refused() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::drain_status_read::drain_status_for_uncovered_workspace_is_refused_as_resident_scope_refused",
    ) {
        return;
    }
    let mut fx = fixture();
    let (other_id, other_runtime, _keep_other) = add_host_workspace(&mut fx);
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-drain-status-scope");
    fx.begin_cycle(&run, Utc::now());

    fx.as_cycle_agent(&run, || {
        let unknown_err = drain_status(&fx.runtime, "ws_not_granted", McpCapability::Agent)
            .expect_err("unknown workspace outside mandate");
        assert!(
            unknown_err.contains("resident_scope_refused"),
            "{unknown_err}"
        );

        let other_direct_err = drain_status(&fx.runtime, &other_id, McpCapability::Agent)
            .expect_err("uncovered registered workspace from home runtime");
        assert!(
            other_direct_err.contains("resident_scope_refused"),
            "{other_direct_err}"
        );

        let other_routed_err = drain_status(&other_runtime, &other_id, McpCapability::Agent)
            .expect_err("uncovered registered workspace routed to its runtime");
        assert!(
            other_routed_err.contains("resident_scope_refused"),
            "{other_routed_err}"
        );
    });
}

#[test]
fn drain_controls_from_a_cycle_and_status_outside_a_cycle_keep_operator_requirement() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::drain_status_read::drain_controls_from_a_cycle_and_status_outside_a_cycle_keep_operator_requirement",
    ) {
        return;
    }
    let fx = fixture();
    let workspace = fx.runtime.workspace_id().unwrap();
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-drain-controls-refused");
    fx.begin_cycle(&run, Utc::now());

    // 1. Drain control actions from a live cycle holding `dispatch` stay refused.
    fx.as_cycle_agent(&run, || {
        for (label, input) in [
            (
                "start",
                json!({"action": "auto", "mode": "start", "for_seconds": 60, "workspace": workspace}),
            ),
            (
                "stop",
                json!({"action": "auto", "mode": "stop", "workspace": workspace}),
            ),
            (
                "resize",
                json!({"action": "auto", "mode": "resize", "concurrency": 2, "workspace": workspace}),
            ),
        ] {
            let error = call(&fx.runtime, "orbit.workflow", input, McpCapability::Agent)
                .expect_err("drain control stays operator-only");
            assert!(
                error.contains("capability denied:") && error.contains("operator"),
                "{label}: {error}"
            );
        }
    });
    assert!(
        fx.runtime.workspace_drain_intent().unwrap().is_none(),
        "refused drain controls must write no drain intent"
    );

    // 2. Outside a live resident cycle, non-operator callers stay refused.
    // (a) Ordinary agent session with no run binding.
    let ordinary_err = drain_status(&fx.runtime, &workspace, McpCapability::Agent)
        .expect_err("ordinary agent without operator capability is refused");
    assert!(
        ordinary_err.contains("capability denied:") && ordinary_err.contains("operator"),
        "{ordinary_err}"
    );

    // (b) Another managed run (`agent_explore_pipeline`).
    let KeyedJobRunAdmission::Admitted(other) = fx
        .jobs
        .insert_automation_job_run("agent_explore_pipeline", json!({}), "not-a-cycle-drain")
        .unwrap()
    else {
        panic!("a fresh run key admits its run");
    };
    fx.as_cycle_agent(&other.run_id, || {
        let error = drain_status(&fx.runtime, &workspace, McpCapability::Agent)
            .expect_err("non-resident managed run is refused");
        assert!(
            error.contains("capability denied:") && error.contains("operator"),
            "{error}"
        );
    });

    // (c) A finished (no longer live) resident cycle run.
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.as_cycle_agent(&run, || {
        let error = drain_status(&fx.runtime, &workspace, McpCapability::Agent)
            .expect_err("finished resident cycle is refused");
        assert!(
            error.contains("capability denied:") && error.contains("operator"),
            "{error}"
        );
    });

    // (d) A cycle run that never started (`observe` never granted a live cycle).
    let unstarted = fx.cycle_run(&mandate, "resident-drain-unstarted");
    fx.as_cycle_agent(&unstarted, || {
        let error = drain_status(&fx.runtime, &workspace, McpCapability::Agent)
            .expect_err("unstarted cycle run is refused");
        assert!(
            error.contains("capability denied:") && error.contains("operator"),
            "{error}"
        );
    });

    // 3. An operator caller outside a cycle still reads status with `controls_authorized: true`.
    let operator_reply = drain_status(&fx.runtime, &workspace, McpCapability::Operator)
        .expect("operator caller still reads drain status");
    assert_status_payload(&operator_reply, &workspace, true);
}
