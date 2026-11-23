//! `input.dependency_delivery` reaches the leaf's worktree setup through the
//! shipped `task_auto_pipeline` → `task_gate_pipeline` → leaf chain.
//!
//! The dependency's marked commit sits on a side branch while the base holds
//! the same work squash-merged under a message with no `[ORB-…]` marker, so
//! the delivery check refuses the dependent task unless the escape hatch
//! arrives at the leaf. Each parent runs its shipped steps against scripted
//! pipeline-action answers; its dispatch input feeds the next level, and the
//! leaf runs the shipped `worktree` step against the real Git fixture.
use super::*;
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{execute_job_with_resume, resolve_job_catalog_refs_for_execution};
use orbit_types::task::{TaskRelation, TaskRelationType};
use orbit_types::workflow::activity_job::JobV2;

const TASK: &str = "ORB-333";
const DEPENDENCY: &str = "ORB-222";
const LEAF: &str = "task_local_pipeline";

/// Answers the pipeline actions a drain parent and its gate call, and records
/// the child each `invoke_and_wait` dispatches.
#[derive(Default)]
struct DispatchHost {
    dispatched: Mutex<Vec<(String, Value)>>,
}

impl DispatchHost {
    fn dispatched(&self, job_name: &str) -> Value {
        let dispatched = self.dispatched.lock().unwrap();
        let mut runs = dispatched.iter().filter(|(job, _)| job == job_name);
        let (_, input) = runs
            .next()
            .unwrap_or_else(|| panic!("no {job_name} dispatch"));
        assert!(runs.next().is_none(), "one {job_name} dispatch expected");
        input.clone()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for DispatchHost {
    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Ok(match action {
            "list_backlog_tasks" => json!({
                "tasks": [], "bundles": [[TASK]], "task_ids": [TASK], "excluded": [],
            }),
            "validate_bundles" => json!({"bundles": [[TASK]], "bundle_count": 1}),
            "resolve_delivery_job" => json!({"job_name": LEAF}),
            "reserve_locks" => json!({
                "reserved": true, "reservation_id": "fixture-reservation",
                "footprint_deferred": false, "conflicts": [], "waiting_on_deps": [],
            }),
            "gate_starvation_defer" => json!({"deferred": false, "reservation": {
                "reservation_id": "fixture-reservation", "footprint_deferred": false
            }}),
            "invoke_and_wait" => {
                self.dispatched.lock().unwrap().push((
                    input["job_name"].as_str().unwrap().to_string(),
                    input["run_input"].clone(),
                ));
                json!({"status": "success"})
            }
            "release_locks" | "pipeline_success_guard" => json!({}),
            other => panic!("unexpected action: {other}"),
        })
    }
}

/// A shipped job and the catalog entries its first `steps` steps name.
fn shipped_job(name: &str, activities: &[&str], steps: usize) -> JobV2 {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-core/assets");
    let mut job =
        load_job_asset(&fs::read_to_string(assets.join(format!("jobs/{name}.yaml"))).unwrap())
            .unwrap()
            .spec;
    let mut catalog = V2ActivityCatalog::new();
    for activity in activities {
        let asset = load_activity_asset(
            &fs::read_to_string(assets.join(format!("activities/{activity}.yaml"))).unwrap(),
        )
        .unwrap();
        catalog.insert(*activity, asset.spec);
    }
    // Later steps and recovery hooks need providers and sit outside what is
    // forwarded.
    job.steps.truncate(steps);
    job.final_recovery_activity = None;
    job.steering_gate = None;
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    job
}

fn run(
    job: &JobV2,
    input: Value,
    run_id: &str,
    host: &dyn RuntimeHost,
) -> Result<orbit_engine::JobOutcome, DispatchError> {
    let logs = TempDir::new().unwrap();
    let writer = Arc::new(V2AuditWriter::new(
        run_id,
        "fixture",
        Arc::new(InMemorySink::new(logs.path())),
    ));
    execute_job_with_resume(job, input, run_id, writer, host, None)
}

/// Ship `TASK` through the chain, with `dependency_delivery` on the parent's
/// input when given, and return the leaf's outcome.
fn ship(
    dependency_delivery: Option<&str>,
) -> (
    Result<orbit_engine::JobOutcome, DispatchError>,
    Fixture,
    LifecycleHost,
) {
    let (fixture, host) = Fixture::hosted();
    let repo = &fixture.repo;
    host.add_task(DEPENDENCY, TaskStatus::Done);
    host.add_task(TASK, TaskStatus::Backlog);
    host.edit_task(TASK, |task| {
        task.relations.push(TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: DEPENDENCY.to_string(),
            regression: None,
        });
    });
    // The marked commit lives on the dependency's own branch; the base holds
    // the same work as an unmarked squash commit.
    git(repo, &["checkout", "-b", "dependency"]);
    fs::write(repo.join("dependency-payload"), "dependency\n").unwrap();
    git(repo, &["add", "dependency-payload"]);
    git(
        repo,
        &["commit", "-m", &format!("feat: dependency [{DEPENDENCY}]")],
    );
    git(repo, &["checkout", BASE]);
    fs::write(repo.join("dependency-payload"), "dependency\n").unwrap();
    git(repo, &["add", "dependency-payload"]);
    git(repo, &["commit", "-m", "feat: dependency (#18)"]);

    let mut input = json!({
        "task_ids": [TASK], "mode": "local", "base_branch": BASE, "base_sync": "local",
    });
    if let Some(mode) = dependency_delivery {
        input["dependency_delivery"] = json!(mode);
    }
    let dispatch = DispatchHost::default();
    let auto = shipped_job(
        "task_auto_pipeline",
        &[
            "list_backlog_tasks",
            "validate_bundles",
            "invoke_and_wait",
            "pipeline_success_guard",
        ],
        usize::MAX,
    );
    assert!(
        run(&auto, input, "jrun-delivery-auto", &dispatch)
            .unwrap()
            .success
    );
    let gate = shipped_job(
        "task_gate_pipeline",
        &[
            "resolve_delivery_job",
            "reserve_locks",
            "sleep",
            "gate_starvation_defer",
            "invoke_and_wait",
            "release_locks",
            "pipeline_success_guard",
        ],
        usize::MAX,
    );
    let gate_input = dispatch.dispatched("task_gate_pipeline");
    assert!(
        run(&gate, gate_input, "jrun-delivery-gate", &dispatch)
            .unwrap()
            .success
    );

    // The leaf keeps its shipped worktree-setup step; later steps need
    // providers and are not part of what is forwarded.
    let leaf = shipped_job(LEAF, &["worktree_setup"], 1);
    assert_eq!(leaf.steps[0].id, "worktree");
    let outcome = run(
        &leaf,
        dispatch.dispatched(LEAF),
        "jrun-delivery-leaf",
        &host,
    );
    (outcome, fixture, host)
}

#[test]
fn ship_input_dependency_delivery_ignore_reaches_the_leaf_worktree_setup() {
    isolated_in(
        module_path!(),
        "ship_input_dependency_delivery_ignore_reaches_the_leaf_worktree_setup",
        || {
            let (refused, _fixture, host) = ship(None);
            let refusal = refused.expect_err("an unmarked delivery refuses by default");
            assert!(
                refusal.to_string().contains("not delivered into base")
                    && refusal.to_string().contains("--ignore-dependency-delivery"),
                "{refusal}"
            );
            assert_eq!(host.get_task(TASK).unwrap().status, TaskStatus::Backlog);

            let (admitted, _fixture, host) = ship(Some("ignore"));
            let admitted = admitted.expect("the hatch reaches the leaf's worktree setup");
            assert!(admitted.success, "{admitted:?}");
            assert_eq!(host.get_task(TASK).unwrap().status, TaskStatus::InProgress);
        },
    );
}
