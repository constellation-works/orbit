use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::compose::global_policy_def_store;
use orbit_types::policy::PolicyDef;
use std::collections::HashMap;
use tempfile::tempdir;

fn baseline_def(name: &str) -> PolicyDef {
    let now = Utc::now();
    PolicyDef {
        name: name.to_string(),
        description: Some("test policy".to_string()),
        deny_read: Vec::new(),
        deny_modify: Vec::new(),
        fs_profiles: HashMap::new(),
        created_at: Some(now),
        updated_at: Some(now),
    }
}

#[test]
fn rejects_traversal_policy_name_without_external_write() {
    if !crate::support::isolated("policy::rejects_traversal_policy_name_without_external_write") {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let store = global_policy_def_store(dir.path().join("policies"));

    let err = store
        .upsert_policy_def(&baseline_def("../x"))
        .expect_err("traversal name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());

    let err = store
        .get_policy_def("../x")
        .expect_err("traversal lookup must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

#[test]
fn rejects_traversal_policy_metadata_name_when_loading() {
    if !crate::support::isolated("policy::rejects_traversal_policy_metadata_name_when_loading") {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let policies_dir = dir.path().join("policies");
    std::fs::create_dir_all(&policies_dir).expect("mkdir");
    std::fs::write(
        policies_dir.join("bad.yaml"),
        "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: ../x\nspec: {}\n",
    )
    .expect("seed");

    let store = global_policy_def_store(policies_dir);
    let err = store
        .list_policy_defs()
        .expect_err("traversal metadata name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());
}
