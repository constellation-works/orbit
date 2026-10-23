//  Existing task mutation cases through exported workspace backends.

#[path = "task_mutations/concurrency.rs"]
mod concurrency;
#[path = "task_mutations/creation_grant.rs"]
mod creation_grant;
#[path = "task_mutations/jsonl.rs"]
mod jsonl;

fn store(temp: &TempDir) -> WorkspaceTaskBackends {
    let registry =
        TaskRegistryStore::open(&task_registry_path(&std::env::current_dir().unwrap())).unwrap();
    let repo = temp.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).unwrap();
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("orbit-test-123456".into()),
            slug: "Orbit Test repo".into(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .unwrap();
    workspace_task_backends(registry, binding.partition_id)
}

// Disk snapshots are fixture evidence, not a copy of the bundle reader.
fn bundle_path(id: &str) -> std::path::PathBuf {
    let registry =
        TaskRegistryStore::open(&task_registry_path(&std::env::current_dir().unwrap())).unwrap();
    registry
        .find_task_binding(id)
        .unwrap()
        .unwrap()
        .canonical_path
}
struct Snapshot {
    envelope: TaskEnvelopeV2,
    events: Vec<TaskEventRowV2>,
}
fn snapshot(id: &str) -> Snapshot {
    let path = bundle_path(id);
    Snapshot {
        envelope: serde_yaml::from_slice(
            &std::fs::read(path.join(TASK_ENVELOPE_FILE_NAME)).unwrap(),
        )
        .unwrap(),
        events: std::fs::read_to_string(path.join(TASK_EVENTS_FILE_NAME))
            .unwrap()
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect(),
    }
}

#[path = "task_mutations/repair_gate.rs"]
mod repair_gate;
