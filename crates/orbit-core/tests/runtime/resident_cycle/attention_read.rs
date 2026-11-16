//! A live resident cycle reads the runtime attention kinds (`run_failed`,
//! `run_stuck`, `claim_stale`) and `host_unhealthy` for the workspaces its
//! mandate covers, and only those. Nothing else changes: another workspace
//! and an ordinary agent session still get `capability` degraded entries, and
//! the cycle's writes stay operator-only (ORB-25254).

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_core::{FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope};

use super::*;

const RUNTIME_KINDS: [&str; 4] = ["run_failed", "run_stuck", "claim_stale", "host_unhealthy"];

/// Two registered workspaces: the cycle's own and one its mandate never names.
struct TwoWorkspaces {
    targets: Vec<FederatedWorkspaceTarget>,
    runtimes: Vec<OrbitRuntime>,
}

impl WorkspaceCatalog for TwoWorkspaces {
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
        Ok(self.runtimes[index].clone().into_kernel())
    }
}

/// The fixture's runtime, now registered beside a second workspace. Returns
/// the other workspace's id and the keep-alive for its checkout.
fn register_other_workspace(fx: &mut Fixture) -> (String, TempDir) {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(repo.join(".orbit/config.toml"), CONFIG).unwrap();
    let other = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let home = fx.runtime.clone();
    let targets = vec![
        FederatedWorkspaceTarget {
            workspace_id: home.workspace_id().unwrap(),
            name: home.workspace_label(),
            repo_root: fx.repo.clone(),
        },
        FederatedWorkspaceTarget {
            workspace_id: other.workspace_id().unwrap(),
            name: "other".into(),
            repo_root: repo,
        },
    ];
    let other_id = targets[1].workspace_id.clone();
    assert_ne!(targets[0].workspace_id, other_id);
    fx.runtime = fx
        .runtime
        .clone()
        .with_workspace_catalog(Arc::new(TwoWorkspaces {
            targets,
            runtimes: vec![home, other],
        }));
    (other_id, root)
}

/// A failed delivery run for `task`, newer than the cycle's own run.
fn failed_run(fx: &Fixture, task: &str) {
    let KeyedJobRunAdmission::Admitted(run) = fx
        .jobs
        .insert_automation_job_run("delivery_pipeline", json!({"task_ids": [task]}), "failed-1")
        .unwrap()
    else {
        panic!("a fresh run key admits its run");
    };
    fx.stop_run(&run.run_id, JobRunState::Failed);
}

fn attention(fx: &Fixture, input: Value) -> Value {
    call(&fx.runtime, "orbit.attention", input, McpCapability::Agent).unwrap()
}

/// `degraded` entries with code `capability`, as (scope, kind).
fn capability_degraded(response: &Value) -> Vec<(String, String)> {
    response["degraded"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["code"] == "capability")
        .map(|entry| {
            (
                entry["scope"].as_str().unwrap().to_string(),
                entry["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn denied_kinds(response: &Value, workspace: &str) -> Vec<String> {
    let mut kinds = capability_degraded(response)
        .into_iter()
        .filter(|(scope, kind)| {
            kind != "host_unhealthy" && scope.contains(workspace)
                || kind == "host_unhealthy" && scope.starts_with("host:")
        })
        .map(|(_, kind)| kind)
        .collect::<Vec<_>>();
    kinds.sort();
    kinds
}

#[test]
fn a_live_cycle_reads_runtime_attention_for_its_covered_workspace() {
    let fx = {
        if !super::super::dispatch_admission::isolated(
            "resident_cycle::attention_read::a_live_cycle_reads_runtime_attention_for_its_covered_workspace",
        ) {
            return;
        }
        fixture()
    };
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-attention");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    failed_run(&fx, &watched);
    let input = json!({"kind": RUNTIME_KINDS});

    // Without the cycle's run binding this is an ordinary agent session.
    let ordinary = attention(&fx, input.clone());
    assert_eq!(
        capability_degraded(&ordinary).len(),
        RUNTIME_KINDS.len(),
        "{ordinary}"
    );

    let response = fx.as_cycle_agent(&run, || attention(&fx, input.clone()));
    assert!(capability_degraded(&response).is_empty(), "{response}");
    assert!(
        response["items"].as_array().unwrap().iter().any(|item| {
            item["kind"] == "run_failed" && item["refs"]["task_id"] == watched.as_str()
        }),
        "the failed run is visible to the cycle: {response}"
    );

    // A cycle run that holds no live cycle has no grant to read under.
    let idle = fx.cycle_run(&mandate, "resident-cycle-attention-idle");
    let response = fx.as_cycle_agent(&idle, || attention(&fx, input.clone()));
    assert_eq!(
        capability_degraded(&response).len(),
        RUNTIME_KINDS.len(),
        "a run without a live cycle reads nothing extra: {response}"
    );
}

#[test]
fn a_live_cycle_attention_read_preserves_a_pending_task_commit() {
    let fx = {
        if !super::super::dispatch_admission::isolated(
            "resident_cycle::attention_read::a_live_cycle_attention_read_preserves_a_pending_task_commit",
        ) {
            return;
        }
        fixture()
    };
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-attention-pending");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    failed_run(&fx, &watched);
    let workspace = fx.runtime.workspace_id().unwrap();
    let tasks_before = fx.runtime.observe_task_metadata().unwrap();
    let claims_before = fx.runtime.inspect_execution_claims().unwrap();
    let connection = rusqlite::Connection::open(fx.global.join("orbit.db")).unwrap();
    // A dead committer's prepared row must await an ordinary operational
    // read. Insert it only after the live cycle and its grant are recorded.
    connection
        .execute(
            "INSERT INTO task_commit_journal \
                 (journal_id, workspace_id, task_id, state, intent_json, created_at) \
             VALUES (?1, ?2, ?3, 'prepared', '{}', ?4)",
            rusqlite::params![
                "resident-attention-pending",
                workspace,
                mandate,
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
    let journal_state = || -> String {
        connection
            .query_row(
                "SELECT state FROM task_commit_journal WHERE journal_id = ?1",
                ["resident-attention-pending"],
                |row| row.get(0),
            )
            .unwrap()
    };
    // Inspect raw coordination rows because the public claim inspection
    // correctly refuses to expose claims while the partition is pending.
    let coordination_rows = || {
        connection
            .prepare(
                "SELECT kind, row_id, payload_json, journal_id, created_at \
                 FROM task_coordination_rows WHERE workspace_id = ?1 ORDER BY kind, row_id",
            )
            .unwrap()
            .query_map([&workspace], |row| {
                (0..5)
                    .map(|column| row.get::<_, String>(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let coordination_before = coordination_rows();

    // Scope derivation runs even without runtime kinds in the selection.
    for input in [
        json!({"kind": "pilot_hold"}),
        json!({"kind": RUNTIME_KINDS}),
    ] {
        let response = fx.as_cycle_agent(&run, || attention(&fx, input.clone()));
        assert_eq!(
            journal_state(),
            "prepared",
            "resident attention must not settle the pending commit: {response}"
        );
        assert_eq!(fx.runtime.observe_task_metadata().unwrap(), tasks_before);
        assert_eq!(coordination_rows(), coordination_before);
        assert!(fx.runtime.inspect_execution_claims().is_err());
        assert!(
            response["items"].as_array().unwrap().is_empty(),
            "{response}"
        );
        if input["kind"].is_array() {
            assert_eq!(
                capability_degraded(&response).len(),
                RUNTIME_KINDS.len(),
                "{response}"
            );
        } else {
            assert!(
                response["degraded"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        entry["kind"] == "pilot_hold" && entry["code"] == "source_unavailable"
                    }),
                "{response}"
            );
        }
    }

    // Ordinary reads still recover; the live cycle can then read the same
    // runtime sources without capability degradation.
    fx.runtime.get_task_history(&mandate).unwrap();
    assert_eq!(journal_state(), "aborted");
    assert_eq!(
        fx.runtime.inspect_execution_claims().unwrap(),
        claims_before
    );
    let response = fx.as_cycle_agent(&run, || attention(&fx, json!({"kind": RUNTIME_KINDS})));
    assert!(capability_degraded(&response).is_empty(), "{response}");
    assert!(
        response["items"].as_array().unwrap().iter().any(|item| {
            item["kind"] == "run_failed" && item["refs"]["task_id"] == watched.as_str()
        }),
        "{response}"
    );
}

#[test]
fn runtime_attention_stays_withheld_outside_the_mandate() {
    let mut fx = {
        if !super::super::dispatch_admission::isolated(
            "resident_cycle::attention_read::runtime_attention_stays_withheld_outside_the_mandate",
        ) {
            return;
        }
        fixture()
    };
    let (other, _checkout) = register_other_workspace(&mut fx);
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-attention-scope");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    let home = fx.runtime.workspace_id().unwrap();
    let input = json!({"kind": ["run_failed", "run_stuck", "claim_stale"], "all": true});

    let response = fx.as_cycle_agent(&run, || attention(&fx, input.clone()));
    assert_eq!(
        denied_kinds(&response, &other),
        ["claim_stale", "run_failed", "run_stuck"],
        "the workspace the mandate never names stays withheld: {response}"
    );
    assert!(
        denied_kinds(&response, &home).is_empty(),
        "the covered workspace is read: {response}"
    );

    // Host health is one host-level answer: it is reported withheld while any
    // selected workspace is outside the mandate.
    let response = fx.as_cycle_agent(&run, || {
        attention(&fx, json!({"kind": "host_unhealthy", "all": true}))
    });
    assert_eq!(capability_degraded(&response).len(), 1, "{response}");
    let response = fx.as_cycle_agent(&run, || {
        attention(&fx, json!({"kind": "host_unhealthy", "workspace": home}))
    });
    assert!(capability_degraded(&response).is_empty(), "{response}");
}

#[test]
fn the_attention_read_grants_a_cycle_no_write_capability() {
    let fx = {
        if !super::super::dispatch_admission::isolated(
            "resident_cycle::attention_read::the_attention_read_grants_a_cycle_no_write_capability",
        ) {
            return;
        }
        fixture()
    };
    let watched = fx.task("Watched");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch", "promote", "merge"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-attention-writes");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    let workspace = fx.runtime.workspace_id().unwrap();

    fx.as_cycle_agent(&run, || {
        let read = attention(&fx, json!({"kind": "run_failed"}));
        assert!(capability_degraded(&read).is_empty(), "{read}");
        for (tool, input) in [
            (
                "orbit.task.requeue",
                json!({"id": watched, "workspace": workspace}),
            ),
            (
                "orbit.workflow.run.resume",
                json!({"run_id": run, "workspace": workspace}),
            ),
        ] {
            let error = call(&fx.runtime, tool, input, McpCapability::Agent)
                .expect_err("a resident cycle holds no operator capability");
            assert!(error.contains("capability"), "{tool}: {error}");
        }
    });
}
