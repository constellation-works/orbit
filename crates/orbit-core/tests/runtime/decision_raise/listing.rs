//! Regression for the reproduced per-process decision bundle scan cost.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::{Fixture, raise_input};

const ROOT: &str = "ORBIT_DECISION_LISTING_ROOT";
const PHASE: &str = "ORBIT_DECISION_LISTING_PHASE";
const ROW: &str = "fresh_processes_reuse_decision_proofs";

#[derive(Clone, Default)]
struct BundleReads {
    canonical: Arc<AtomicUsize>,
    lightweight: Arc<AtomicUsize>,
    history: Arc<AtomicUsize>,
    comments: Arc<AtomicUsize>,
}

impl<S: tracing::Subscriber> Layer<S> for BundleReads {
    fn on_new_span(
        &self,
        span: &tracing::span::Attributes<'_>,
        _: &tracing::Id,
        _: Context<'_, S>,
    ) {
        match span.metadata().name() {
            "task_bundle_canonical_read" => {
                self.canonical.fetch_add(1, Ordering::Relaxed);
            }
            "task_bundle_lightweight_read" => {
                self.lightweight.fetch_add(1, Ordering::Relaxed);
            }
            "task_history_read" => {
                self.history.fetch_add(1, Ordering::Relaxed);
            }
            "task_comments_read" => {
                self.comments.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

fn counted<T>(expected: usize, phase: &str, read: impl FnOnce() -> T) -> T {
    let counts = BundleReads::default();
    let value = tracing::subscriber::with_default(Registry::default().with(counts.clone()), read);
    let canonical = counts.canonical.load(Ordering::Relaxed);
    let lightweight = counts.lightweight.load(Ordering::Relaxed);
    let history = counts.history.load(Ordering::Relaxed);
    let comments = counts.comments.load(Ordering::Relaxed);
    writeln!(
        std::io::stderr(),
        "test-note: decision read counts: {phase}: canonical={canonical}, lightweight={lightweight}, history={history}, comments={comments}, expected={expected}"
    ).expect("decision read count evidence");
    assert_eq!(
        (canonical, lightweight),
        (0, expected),
        "decision scan must read each unproven bundle once and reuse closed proofs across processes: {phase}"
    );
    assert_eq!(
        (history, comments),
        (0, 0),
        "snapshot reads must not reread logs separately: {phase}"
    );
    value
}

fn run_phase(root: &Path, phase: &str) {
    let test = "decision_raise::listing::mutable_scenarios";
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("ORBIT_RUNTIME_SCENARIO_ROW", ROW)
        .env(ROOT, root)
        .env(PHASE, phase)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .current_dir(root);
    let output = orbit_common::test_env::run_child_test(&mut command, test, root);
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
}

fn seed_pilot_receipt(fx: &Fixture, task: &str) {
    use orbit_store::contracts::{TaskArtifactUpdateParams, TaskHistoryUpdateParams};
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::{TaskArtifact, TaskComment};
    let registry = TaskRegistryStore::open(&task_registry_path(&fx.global)).unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        fx.runtime.workspace_id().unwrap(),
        fx.runtime.sqlite_store().unwrap(),
    )
    .unwrap();
    let content = serde_json::to_vec(&json!({"assessment": {"disposition": "ready"}})).unwrap();
    let reference = json!({
        "assessment_artifact": "pilot/fixture.json",
        "assessment_sha256": orbit_common::security::release::sha256_hex(&content),
    });
    backends
        .task
        .artifact
        .upsert_task_artifacts(
            task,
            TaskArtifactUpdateParams {
                actor: "task-pilot".into(),
                upsert_artifacts: vec![TaskArtifact {
                    path: "pilot/fixture.json".into(),
                    media_type: "application/json".into(),
                    content,
                    created_by: Some("task-pilot".into()),
                }],
                ..Default::default()
            },
        )
        .unwrap();
    backends
        .task
        .history
        .update_task_history(
            task,
            TaskHistoryUpdateParams {
                actor: "task-pilot".into(),
                append_comments: vec![TaskComment {
                    at: chrono::Utc::now(),
                    by: "task-pilot".into(),
                    message: format!("operation_id=fixture-pilot\n{reference}"),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

fn fresh_processes_reuse_decision_proofs() {
    if let Some(root) = std::env::var_os(ROOT) {
        let root = std::path::PathBuf::from(root);
        let phase = std::env::var(PHASE).unwrap();
        let spec: Value =
            serde_json::from_slice(&std::fs::read(root.join("listing.json")).unwrap()).unwrap();
        let runtime =
            OrbitRuntime::from_roots(&root.join("global"), &root.join("repo/.orbit")).unwrap();
        let expected = match phase.as_str() {
            "cold" => 5,
            "warm" => 1,
            "changed" => 2,
            "corrupt" => 3,
            _ => panic!("unknown phase"),
        };
        let listed = counted(expected, &phase, || {
            runtime.run_tool("orbit.decision.list", json!({})).unwrap()
        });
        let expected_open = if phase == "changed" || phase == "corrupt" {
            2
        } else {
            1
        };
        assert_eq!(listed["count"], expected_open, "{listed}");
        if phase == "cold" {
            std::fs::write(root.join("open.json"), serde_json::to_vec(&listed).unwrap()).unwrap();
        } else if phase == "warm" {
            let earlier: Value =
                serde_json::from_slice(&std::fs::read(root.join("open.json")).unwrap()).unwrap();
            assert_eq!(
                listed, earlier,
                "fresh-process cache preserves the exact tool envelope"
            );
            let closed = counted(1, "include closed", || {
                runtime
                    .run_tool("orbit.decision.list", json!({"include_closed": true}))
                    .unwrap()
            });
            assert_eq!(closed["count"], 2, "{closed}");
            assert!(
                closed["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|request| request["id"] == spec["closed_request"]
                        && request["state"] == "answered")
            );
            let request = counted(1, "owner lookup", || {
                runtime
                    .get_decision_request(spec["closed_request"].as_str().unwrap())
                    .unwrap()
            })
            .unwrap();
            assert_eq!(request.task_id, spec["closed_task"].as_str().unwrap());
        }
        if phase == "corrupt" {
            assert_eq!(listed["warnings"].as_array().unwrap().len(), 1, "{listed}");
            assert!(
                runtime.list_decision_requests(None, None, false).is_err(),
                "strict scans still reject corrupt bundles"
            );
            let request = counted(1, "lookup bypasses unrelated corrupt task", || {
                runtime
                    .get_decision_request(spec["closed_request"].as_str().unwrap())
                    .unwrap()
            });
            assert!(request.is_some());
        }
        return;
    }

    let fx = Fixture::new();
    let empty = fx.task("No decisions", "sol");
    fx.runtime
        .update_task_with_identity(
            &empty,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                    "checks/fixture.log",
                    "passed",
                )],
                ..Default::default()
            },
            None,
            Some("codex".into()),
        )
        .unwrap();
    let piloted = fx.task("A piloted task without decisions", "sol");
    seed_pilot_receipt(&fx, &piloted);
    let changed = fx.task("Decision raised later", "dk");
    let open = fx.task("Open decision", "dk");
    fx.raise("codex", raise_input(&open, "Which open option?"))
        .unwrap();
    let closed = fx.task("Answered decision", "dk");
    let raised = fx
        .raise("codex", raise_input(&closed, "Which closed option?"))
        .unwrap();
    let request_id = raised["id"].as_str().unwrap();
    fx.operator()
        .answer_decision_request_as_human(request_id, "1", None, "human:operator".into(), None)
        .unwrap();
    assert_eq!(
        fx.runtime.get_task(&closed).unwrap().status,
        orbit_types::task::TaskStatus::Done
    );
    let root = fx._root.path();
    std::fs::write(
        root.join("listing.json"),
        serde_json::to_vec(&json!({"closed_request": request_id, "closed_task": closed})).unwrap(),
    )
    .unwrap();
    // Creation and raising can themselves list decisions. Clear only this
    // fixture's derived cache so the first independent process is unproven.
    let connection = rusqlite::Connection::open(fx.global.join("tasks/index.sqlite")).unwrap();
    connection
        .execute("DELETE FROM task_decision_request_owners", [])
        .unwrap();
    connection
        .execute("DELETE FROM task_decision_snapshots", [])
        .unwrap();
    run_phase(root, "cold");
    run_phase(root, "warm");
    fx.raise("codex", raise_input(&changed, "Which new option?"))
        .unwrap();
    run_phase(root, "changed");

    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&fx.global),
    )
    .unwrap();
    let bundle = registry
        .canonical_task_bundle_path(&fx.runtime.workspace_id().unwrap(), &empty)
        .unwrap();
    let file = fx
        .runtime
        .get_task_artifact_manifest(&empty)
        .unwrap()
        .remove(0);
    std::fs::remove_file(bundle.join("artifacts").join(file.blob)).unwrap();
    run_phase(root, "corrupt");
}

crate::scenario_table!(mutable_scenarios, fresh_processes_reuse_decision_proofs);
