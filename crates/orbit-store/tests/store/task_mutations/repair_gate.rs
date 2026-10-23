use super::*;
use orbit_types::task::{TaskRelation, TaskRelationType};
const WORKSPACE: &str = "orbit-test-123456";
const RETIRED_WORKSPACE: &str = "orbit-test-654321";
const FOREIGN_TARGET: &str = "ZZZ-00001";
const HEALTHY_TASKS: usize = 5;
struct Fixture {
    _temp: TempDir,
    registry: TaskRegistryStore,
    source: String,
    target: String,
}
impl Fixture {
    fn sql(&self, statement: &str) {
        rusqlite::Connection::open(task_registry_path(&std::env::current_dir().unwrap()))
            .unwrap()
            .execute_batch(statement)
            .unwrap();
    }
}
pub(super) fn bound_store(
    registry: &TaskRegistryStore,
    temp: &TempDir,
    partition_id: &str,
    checkout: &str,
) -> WorkspaceTaskBackends {
    bound_store_at(registry, temp.path(), partition_id, checkout)
}

/// [`bound_store`] with the checkout under an explicit `root`, for fixtures
/// that need a root other than a temp directory's own path.
pub(super) fn bound_store_at(
    registry: &TaskRegistryStore,
    root: &std::path::Path,
    partition_id: &str,
    checkout: &str,
) -> WorkspaceTaskBackends {
    let repo_dir = root.join(checkout);
    let orbit_dir = repo_dir.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition_id.to_string()),
            slug: format!("Orbit Test {checkout}"),
            repo_root: repo_dir.clone(),
            workspace_path: repo_dir.clone(),
            orbit_dir: orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind workspace");
    workspace_task_backends(registry.clone(), binding.partition_id)
}

/// The live ORB-14171 shape: a done task in this workspace keeps
/// `spawned_from` to a task whose workspace was retired, its generated
/// relation rows are gone, and the index is stale. A foreign-prefix edge and
/// an ADR `produces` edge stay allowed to dangle.
fn degraded_fixture() -> Fixture {
    let temp = TempDir::new().expect("tempdir");
    let registry = TaskRegistryStore::open(&task_registry_path(&std::env::current_dir().unwrap()))
        .expect("open registry");
    let store = bound_store(&registry, &temp, WORKSPACE, "repo");
    let retired = bound_store(&registry, &temp, RETIRED_WORKSPACE, "bridge");

    let target = retired
        .task
        .create_task(create_params("Retired provenance", TaskStatus::Done))
        .expect("create target")
        .id;
    for index in 0..HEALTHY_TASKS {
        store
            .task
            .create_task(create_params(
                &format!("Healthy {index}"),
                TaskStatus::Backlog,
            ))
            .expect("create healthy task");
    }
    let _in_progress = store
        .task
        .create_task(create_params("Active", TaskStatus::InProgress))
        .expect("create active task")
        .id;
    let mut source = create_params("Spawned", TaskStatus::Done);
    source.relations = vec![
        TaskRelation {
            relation_type: TaskRelationType::SpawnedFrom,
            target: target.clone(),
            regression: None,
        },
        TaskRelation {
            relation_type: TaskRelationType::RelatedTo,
            target: FOREIGN_TARGET.to_string(),
            regression: None,
        },
        TaskRelation {
            relation_type: TaskRelationType::Produces,
            target: "ADR-0001".to_string(),
            regression: None,
        },
    ];
    let source = store.task.create_task(source).expect("create source").id;

    let _target_dir = registry
        .find_task_binding(&target)
        .expect("lookup target")
        .expect("target bound")
        .canonical_path;
    assert!(
        registry
            .unregister_task_bundle(&target, RETIRED_WORKSPACE)
            .expect("retire target"),
        "fixture must drop the target's binding"
    );
    let fixture = Fixture {
        _temp: temp,
        registry,
        source,
        target,
    };
    fixture.sql(&format!(
        "DELETE FROM task_bundle_relations WHERE source_task_id = '{source}';
         DELETE FROM task_bundle_index WHERE task_id = '{source}';",
        source = fixture.source
    ));
    fixture
}

#[test]
fn rebuild_refusal_names_the_canonical_source_edge() {
    if !support::isolated("repair_gate::rebuild_refusal_names_the_canonical_source_edge") {
        return;
    }
    let fixture = degraded_fixture();
    let envelopes = fixture
        .registry
        .tasks_for_workspace(WORKSPACE)
        .unwrap()
        .iter()
        .map(|binding| {
            serde_yaml::from_slice::<TaskEnvelopeV2>(
                &std::fs::read(binding.canonical_path.join(TASK_ENVELOPE_FILE_NAME)).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    let error = fixture
        .registry
        .replace_workspace_task_indexes(WORKSPACE, &envelopes)
        .expect_err("validator stays strict");
    assert!(
        error.to_string().contains(&format!(
            "{} spawned_from -> '{}'",
            fixture.source, fixture.target
        )),
        "the refusal must name the source and relation: {error}"
    );

    let unresolved = fixture
        .registry
        .unresolved_relation_targets(WORKSPACE, &envelopes)
        .expect("audit");
    assert_eq!(
        unresolved
            .iter()
            .map(|edge| (
                edge.source_task_id.as_str(),
                edge.relation_type.as_str(),
                edge.target_task_id.as_str(),
                edge.indexed
            ))
            .collect::<Vec<_>>(),
        vec![(
            fixture.source.as_str(),
            "spawned_from",
            fixture.target.as_str(),
            false
        )],
        "only the locally known task edge is unresolved, and the index lacks it"
    );
}
