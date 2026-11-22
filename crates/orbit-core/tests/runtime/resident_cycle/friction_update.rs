//! A live `resident_cycle_pipeline` run updates friction records (`orbit.friction`
//! action `update`) in its covered workspaces without any granted right (including
//! observe-only cycles), keyed `resident:<mandate>:<cycle>:<n>` on the action
//! ledger; uncovered workspaces are refused as `resident_scope_refused`; any
//! update carrying `rehome_to` is refused as `resident_tool_refused` and leaves
//! the record in its workspace (ORB-26833).

use std::sync::{Arc, OnceLock};

use orbit_common::OrbitError;
use orbit_core::{FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope};
use orbit_types::task::TaskArtifact;
use orbit_types::workflow::activity_job::ActivityV2Spec;

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

fn seed_friction(runtime: &OrbitRuntime, title: &str, body: &str) -> String {
    let added = call(
        runtime,
        "orbit.friction",
        json!({"action": "add", "title": title, "body": body, "model": "codex"}),
        McpCapability::Agent,
    )
    .unwrap();
    added["id"].as_str().unwrap().to_string()
}

fn show_friction(runtime: &OrbitRuntime, id: &str) -> Value {
    runtime
        .run_tool("orbit.friction.show", json!({"id": id}))
        .unwrap()
}

fn list_frictions(runtime: &OrbitRuntime) -> Vec<Value> {
    runtime
        .run_tool("orbit.friction.list", json!({}))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn update_friction(runtime: &OrbitRuntime, input: Value) -> Result<Value, String> {
    call(runtime, "orbit.friction", input, McpCapability::Agent)
}

fn cycle_action_rows(fx: &Fixture, mandate: &str, cycle: &str) -> Vec<ResidentActionRow> {
    let mut rows: Vec<ResidentActionRow> = Vec::new();
    for row in fx
        .runtime
        .get_task_history(mandate)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
        .filter_map(|entry| serde_json::from_str::<ResidentActionRow>(entry.note.as_deref()?).ok())
        .filter(|row| row.cycle == cycle)
    {
        match rows.iter_mut().find(|existing| existing.key == row.key) {
            Some(existing) => *existing = row,
            None => rows.push(row),
        }
    }
    rows
}

fn multi_workspace_mandate(
    fx: &Fixture,
    watched: &str,
    workspaces: &[&str],
    rights: &[&str],
) -> String {
    let mandate = fx.add_task(TaskAddParams {
        title: "[orchestration]: multi-ws friction - sol".into(),
        tags: vec!["resident".into()],
        ..Default::default()
    });
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Coordinate across covered workspaces",
        "exit_criteria": ["All covered work completes"],
        "workspaces": workspaces,
        "watch": {"tasks": [watched], "tags": []},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    fx.update_task(
        &mandate,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "resident/mandate.json",
                body.to_string(),
            )],
            ..Default::default()
        },
    );
    fx.operator_call(
        "orbit.resident.grant",
        json!({"task_id": mandate, "add": rights, "workspaces": workspaces}),
    );
    mandate
}

#[test]
fn resident_cycle_updates_friction_in_covered_workspaces_including_observe_only() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::friction_update::resident_cycle_updates_friction_in_covered_workspaces_including_observe_only",
    ) {
        return;
    }
    let activity = orbit_engine::load_activity_asset(include_str!(
        "../../../assets/activities/resident_cycle.yaml"
    ))
    .unwrap();
    let ActivityV2Spec::AgentLoop(spec) = &activity.spec.spec else {
        panic!("resident_cycle must be an agent_loop activity");
    };
    assert!(
        !spec
            .tool_disallow_list
            .as_ref()
            .is_some_and(|list| list.iter().any(|tool| tool == "orbit.friction:update")),
        "resident_cycle.yaml must no longer disallow orbit.friction:update"
    );

    let mut fx = fixture();
    let (other_id, other_runtime, _keep_other) = add_host_workspace(&mut fx);
    let home_id = fx.runtime.workspace_id().unwrap();
    let watched = fx.task("Watched");

    let home_fric = seed_friction(
        &fx.runtime,
        "Home gate stall",
        "Observed before landing fix.",
    );
    let other_fric = seed_friction(
        &other_runtime,
        "Other workspace defect",
        "Observed in second covered workspace.",
    );

    // 1. Observe-only mandate (zero rights granted) covering both home and other.
    let observe_only_mandate = multi_workspace_mandate(&fx, &watched, &[&home_id, &other_id], &[]);
    let run = fx.cycle_run(&observe_only_mandate, "resident-friction-observe-only");
    let brief = fx.begin_cycle(&run, Utc::now());
    assert_eq!(brief["authority"]["rights"], json!([]), "{brief}");
    let cycle = brief["cycle"].as_str().unwrap().to_string();

    let home_input = json!({
        "action": "update",
        "id": home_fric,
        "status": "resolved",
        "body": "Observed before landing fix.\n\nFix landed in PR #42."
    });
    let (home_updated, other_updated) = fx.as_cycle_agent(&run, || {
        let home_updated = update_friction(&fx.runtime, home_input.clone())
            .expect("observe-only cycle updates friction in home workspace");
        let other_updated = update_friction(
            &other_runtime,
            json!({
                "action": "update",
                "id": other_fric,
                "status": "resolved",
                "workspace": other_id,
                "body": "Observed in second covered workspace.\n\nFix landed in PR #43."
            }),
        )
        .expect("observe-only cycle updates friction in second covered workspace");
        (home_updated, other_updated)
    });

    let expected_key_1 = format!("resident:{observe_only_mandate}:{cycle}:1");
    let expected_key_2 = format!("resident:{observe_only_mandate}:{cycle}:2");
    assert_eq!(
        home_updated["resident_action_key"], expected_key_1,
        "{home_updated}"
    );
    assert_eq!(
        other_updated["resident_action_key"], expected_key_2,
        "{other_updated}"
    );
    assert_eq!(show_friction(&fx.runtime, &home_fric)["status"], "resolved");
    assert_eq!(
        show_friction(&other_runtime, &other_fric)["status"],
        "resolved"
    );

    let rows = cycle_action_rows(&fx, &observe_only_mandate, &cycle);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].key, expected_key_1);
    assert_eq!(rows[0].phase, ResidentActionPhase::Ok);
    assert_eq!(rows[0].subject.as_deref(), Some(home_fric.as_str()));
    assert_eq!(rows[1].key, expected_key_2);
    assert_eq!(rows[1].phase, ResidentActionPhase::Ok);
    assert_eq!(rows[1].subject.as_deref(), Some(other_fric.as_str()));

    // Recovery cycle replays the completed update as `already_done` without repeating.
    fx.stop_run(&run, JobRunState::Failed);
    let recovery = fx.cycle_run(&observe_only_mandate, "resident-friction-recovery");
    let recovery_brief = fx.begin_cycle(&recovery, Utc::now() + Duration::minutes(11));
    assert_eq!(recovery_brief["recovery_of"], cycle, "{recovery_brief}");
    let replayed = fx.as_cycle_agent(&recovery, || {
        update_friction(&fx.runtime, home_input).unwrap()
    });
    assert_eq!(replayed["resident_action"], "already_done", "{replayed}");
    assert_eq!(replayed["key"], expected_key_1, "{replayed}");
    fx.runtime.finish_resident_cycle(&recovery).unwrap();

    // 2. A cycle narrowed to observe-only by a maintenance notice still updates friction.
    let notice_fric = seed_friction(&fx.runtime, "Notice-window friction", "Initial body.");
    let narrowed_mandate = fx.mandate(&watched);
    fx.grant(&narrowed_mandate, &["dispatch", "promote", "merge"]);
    fx.operator_call(
        "orbit.notice.post",
        json!({"kind": "maintenance", "title": "Maintenance", "expires_in_hours": 24}),
    );
    let narrowed_run = fx.cycle_run(&narrowed_mandate, "resident-friction-notice-narrowed");
    let narrowed_brief = fx.begin_cycle(&narrowed_run, Utc::now() + Duration::minutes(22));
    assert_eq!(narrowed_brief["authority"]["observe_only"], true);
    let narrowed_cycle = narrowed_brief["cycle"].as_str().unwrap().to_string();
    let updated = fx.as_cycle_agent(&narrowed_run, || {
        update_friction(
            &fx.runtime,
            json!({
                "action": "update",
                "id": notice_fric,
                "status": "triaged",
                "body": "Initial body.\n\nTriaged during maintenance window."
            }),
        )
        .expect("maintenance-narrowed observe-only cycle may update friction")
    });
    assert_eq!(
        updated["resident_action_key"],
        format!("resident:{narrowed_mandate}:{narrowed_cycle}:1")
    );
    assert_eq!(
        show_friction(&fx.runtime, &notice_fric)["status"],
        "triaged"
    );
}

#[test]
fn friction_update_for_uncovered_workspace_is_refused_as_resident_scope_refused() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::friction_update::friction_update_for_uncovered_workspace_is_refused_as_resident_scope_refused",
    ) {
        return;
    }
    let mut fx = fixture();
    let (other_id, other_runtime, _keep_other) = add_host_workspace(&mut fx);
    let watched = fx.task("Watched");
    let other_fric = seed_friction(
        &other_runtime,
        "Uncovered workspace friction",
        "Must remain open.",
    );

    let mandate = fx.mandate(&watched);
    fx.grant(
        &mandate,
        &["dispatch", "promote", "merge", "plugin_upgrade"],
    );
    let run = fx.cycle_run(&mandate, "resident-friction-scope-refused");
    let brief = fx.begin_cycle(&run, Utc::now());
    let cycle = brief["cycle"].as_str().unwrap().to_string();

    fx.as_cycle_agent(&run, || {
        let unknown_err = update_friction(
            &fx.runtime,
            json!({
                "action": "update",
                "id": other_fric,
                "status": "resolved",
                "workspace": "ws_not_granted"
            }),
        )
        .expect_err("unknown workspace outside mandate");
        assert!(
            unknown_err.contains("resident_scope_refused"),
            "{unknown_err}"
        );

        let home_named_err = update_friction(
            &fx.runtime,
            json!({
                "action": "update",
                "id": other_fric,
                "status": "resolved",
                "workspace": other_id
            }),
        )
        .expect_err("uncovered registered workspace from home runtime");
        assert!(
            home_named_err.contains("resident_scope_refused"),
            "{home_named_err}"
        );

        let other_routed_err = update_friction(
            &other_runtime,
            json!({
                "action": "update",
                "id": other_fric,
                "status": "resolved",
                "workspace": other_id
            }),
        )
        .expect_err("uncovered registered workspace routed to its runtime");
        assert!(
            other_routed_err.contains("resident_scope_refused"),
            "{other_routed_err}"
        );
    });

    let stored = show_friction(&other_runtime, &other_fric);
    assert_eq!(stored["status"], "open");
    assert_eq!(stored["body"], "Must remain open.");
    assert!(
        cycle_action_rows(&fx, &mandate, &cycle).is_empty(),
        "scope-refused calls write no action ledger row"
    );
}

#[test]
fn friction_update_carrying_rehome_to_is_refused_as_resident_tool_refused() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::friction_update::friction_update_carrying_rehome_to_is_refused_as_resident_tool_refused",
    ) {
        return;
    }
    let mut fx = fixture();
    let (other_id, other_runtime, _keep_other) = add_host_workspace(&mut fx);
    let home_id = fx.runtime.workspace_id().unwrap();
    let watched = fx.task("Watched");

    let home_fric = seed_friction(&fx.runtime, "Stay in home", "Must not move.");
    let other_fric = seed_friction(&other_runtime, "Stay in other", "Must not move either.");

    let mandate = multi_workspace_mandate(
        &fx,
        &watched,
        &[&home_id, &other_id],
        &["dispatch", "promote", "merge", "plugin_upgrade"],
    );
    let run = fx.cycle_run(&mandate, "resident-friction-rehome-refused");
    let brief = fx.begin_cycle(&run, Utc::now());
    let cycle = brief["cycle"].as_str().unwrap().to_string();

    fx.as_cycle_agent(&run, || {
        for (label, runtime, input) in [
            (
                "family rehome_to",
                &fx.runtime,
                json!({
                    "action": "update",
                    "id": home_fric,
                    "rehome_to": other_id,
                    "body": "Attempted move"
                }),
            ),
            (
                "rehome_to disposition only",
                &fx.runtime,
                json!({
                    "action": "update",
                    "id": home_fric,
                    "rehome_to": other_id,
                    "move": false
                }),
            ),
            (
                "empty rehome_to",
                &fx.runtime,
                json!({
                    "action": "update",
                    "id": home_fric,
                    "rehome_to": ""
                }),
            ),
            (
                "cross-workspace rehome_to",
                &other_runtime,
                json!({
                    "action": "update",
                    "id": other_fric,
                    "workspace": other_id,
                    "rehome_to": home_id
                }),
            ),
        ] {
            let error = update_friction(runtime, input)
                .expect_err("rehome_to must be refused regardless of granted rights");
            assert!(error.contains("resident_tool_refused"), "{label}: {error}");
        }
    });

    let home_stored = show_friction(&fx.runtime, &home_fric);
    assert_eq!(home_stored["status"], "open");
    assert_eq!(home_stored["body"], "Must not move.");
    assert!(home_stored["rehome_to"].is_null(), "{home_stored}");
    let other_stored = show_friction(&other_runtime, &other_fric);
    assert_eq!(other_stored["status"], "open");
    assert_eq!(other_stored["body"], "Must not move either.");
    assert!(other_stored["rehome_to"].is_null(), "{other_stored}");
    assert_eq!(list_frictions(&fx.runtime).len(), 1);
    assert_eq!(list_frictions(&other_runtime).len(), 1);
    assert!(
        cycle_action_rows(&fx, &mandate, &cycle).is_empty(),
        "tool-refused calls write no action ledger row"
    );

    // After the cycle finishes, updates are refused as stale.
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.as_cycle_agent(&run, || {
        let error = update_friction(
            &fx.runtime,
            json!({"action": "update", "id": home_fric, "status": "resolved"}),
        )
        .expect_err("finished cycle cannot update friction");
        assert!(error.contains("resident_cycle_stale"), "{error}");
    });
}
