use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::compose::global_executor_def_store;
use orbit_types::task::HostOs;
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::ExecutorType;
use std::collections::{BTreeSet, HashMap};
use tempfile::tempdir;

fn baseline_def(name: &str) -> ExecutorDef {
    let now = Utc::now();
    ExecutorDef {
        name: name.to_string(),
        executor_type: ExecutorType::DirectAgent,
        command: Some(name.to_string()),
        args: vec!["--flag".to_string()],
        inspection_args: None,
        stdout_format: None,
        model_pair_override: None,
        model_flag: None,
        timeout_seconds: None,
        auth_probe: None,
        env: HashMap::new(),
        sandbox: None,
        allow_fallback: false,
        host_os: None,
        created_at: Some(now),
        updated_at: Some(now),
    }
}

#[test]
fn rejects_traversal_executor_name_without_external_write() {
    if !crate::support::isolated("executor::rejects_traversal_executor_name_without_external_write")
    {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let store = global_executor_def_store(dir.path().join("executors"));

    let err = store
        .upsert_executor_def(&baseline_def("../x"))
        .expect_err("traversal name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());

    let err = store
        .get_executor_def("../x")
        .expect_err("traversal lookup must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

#[test]
fn rejects_traversal_executor_metadata_name_when_loading() {
    if !crate::support::isolated("executor::rejects_traversal_executor_metadata_name_when_loading")
    {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let executors_dir = dir.path().join("executors");
    std::fs::create_dir_all(&executors_dir).expect("mkdir");
    std::fs::write(
            executors_dir.join("bad.yaml"),
            "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: ../x\nspec:\n  executor_type: direct_agent\n",
        )
        .expect("seed");

    let store = global_executor_def_store(executors_dir);
    let err = store
        .list_executor_defs()
        .expect_err("traversal metadata name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());
}

/// An omitted `host_os` is not written back. A declared set loads as stored.
#[test]
fn omitted_host_os_is_not_serialized_and_a_declared_set_round_trips() {
    if !crate::support::isolated(
        "executor::omitted_host_os_is_not_serialized_and_a_declared_set_round_trips",
    ) {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let store = global_executor_def_store(dir.path().join("executors"));
    store
        .upsert_executor_def(&baseline_def("codex"))
        .expect("unrestricted executor");
    let yaml = std::fs::read_to_string(dir.path().join("executors/codex.yaml")).expect("yaml");
    assert!(
        !yaml.contains("host_os"),
        "omission stays omitted: {yaml}"
    );
    assert!(
        store
            .get_executor_def("codex")
            .expect("get")
            .expect("codex")
            .host_os
            .is_none()
    );

    let mut muse = baseline_def("muse");
    muse.host_os = Some(BTreeSet::from([HostOs::Macos]));
    store.upsert_executor_def(&muse).expect("macos executor");
    assert_eq!(
        store
            .get_executor_def("muse")
            .expect("get")
            .expect("muse")
            .host_os,
        muse.host_os
    );
}

/// An empty list or an unknown name fails the load and names the asset file.
#[test]
fn empty_or_unknown_host_os_is_refused_at_load_and_names_the_asset() {
    if !crate::support::isolated(
        "executor::empty_or_unknown_host_os_is_refused_at_load_and_names_the_asset",
    ) {
        return;
    }
    for (body, needle) in [
        ("host_os: []\n", "host_os must list at least one"),
        ("host_os: [darwin]\n", "unknown host_os `darwin`"),
    ] {
        let dir = tempdir().expect("tempdir");
        let executors = dir.path().join("executors");
        std::fs::create_dir_all(&executors).expect("mkdir");
        let path = executors.join("muse.yaml");
        std::fs::write(
            &path,
            format!(
                "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: muse\nspec:\n  executor_type: direct_agent\n  command: muse\n  {body}"
            ),
        )
        .expect("seed");
        let error = global_executor_def_store(executors)
            .list_executor_defs()
            .expect_err("bad host_os must fail the load");
        let message = error.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "{message}"
        );
        assert!(message.contains(needle), "{message}");
    }
}
