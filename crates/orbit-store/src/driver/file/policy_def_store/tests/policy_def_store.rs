// Migrated from file/policy_def_store.rs per ORB-00231
use super::super::*;
use chrono::Utc;
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
fn roundtrips_valid_policy_name_unchanged() {
    let dir = tempdir().expect("tempdir");
    let store = PolicyDefFileStore::new(dir.path().join("policies"));

    let def = baseline_def("local-policy_1");
    store.upsert_policy_def(&def).expect("upsert");

    let loaded = store
        .get_policy_def("local-policy_1")
        .expect("get")
        .expect("present");
    assert_eq!(loaded.name, "local-policy_1");
    assert!(dir.path().join("policies/local-policy_1.yaml").exists());
}

#[test]
fn yml_only_policy_is_listed_loaded_and_updated_in_place() {
    let dir = tempdir().expect("tempdir");
    let policies_dir = dir.path().join("policies");
    let store = PolicyDefFileStore::new(policies_dir.clone());
    let mut def = baseline_def("default");
    def.deny_read.push("private/**".to_string());
    store.upsert_policy_def(&def).expect("write fixture");
    std::fs::rename(
        policies_dir.join("default.yaml"),
        policies_dir.join("default.yml"),
    )
    .expect("rename fixture");

    assert_eq!(store.list_policy_defs().expect("list"), vec![def.clone()]);
    assert_eq!(
        store.get_policy_def("default").expect("get"),
        Some(def.clone())
    );

    def.deny_modify.push("private/**".to_string());
    store.upsert_policy_def(&def).expect("update yml");
    assert_eq!(
        store.list_policy_defs().expect("list updated"),
        vec![def.clone()]
    );
    assert_eq!(
        store.get_policy_def("default").expect("get updated"),
        Some(def)
    );
    assert!(policies_dir.join("default.yml").exists());
    assert!(!policies_dir.join("default.yaml").exists());
}

#[test]
fn duplicate_extensions_select_yaml_consistently_without_overwriting_yml() {
    let dir = tempdir().expect("tempdir");
    let policies_dir = dir.path().join("policies");
    let store = PolicyDefFileStore::new(policies_dir.clone());
    let mut yaml = baseline_def("default");
    yaml.deny_read.push("yaml-only/**".to_string());
    store.upsert_policy_def(&yaml).expect("write yaml");
    let yml_path = policies_dir.join("default.yml");
    std::fs::write(
        &yml_path,
        "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: default\nspec:\n  denyRead:\n    - yml-only/**\n",
    )
    .expect("write alternate");
    let alternate = std::fs::read(&yml_path).expect("read alternate");

    assert_eq!(store.list_policy_defs().expect("list"), vec![yaml.clone()]);
    assert_eq!(
        store.get_policy_def("default").expect("get"),
        Some(yaml.clone())
    );

    yaml.deny_modify.push("yaml-only/**".to_string());
    store
        .upsert_policy_def(&yaml)
        .expect("update preferred variant");
    assert_eq!(
        store.list_policy_defs().expect("list updated"),
        vec![yaml.clone()]
    );
    assert_eq!(
        store.get_policy_def("default").expect("get updated"),
        Some(yaml)
    );
    assert_eq!(std::fs::read(yml_path).expect("read alternate"), alternate);
}

#[test]
fn rejects_traversal_policy_name_without_external_write() {
    let dir = tempdir().expect("tempdir");
    let store = PolicyDefFileStore::new(dir.path().join("policies"));

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
    let dir = tempdir().expect("tempdir");
    let policies_dir = dir.path().join("policies");
    std::fs::create_dir_all(&policies_dir).expect("mkdir");
    std::fs::write(
        policies_dir.join("bad.yaml"),
        "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: ../x\nspec: {}\n",
    )
    .expect("seed");

    let store = PolicyDefFileStore::new(policies_dir);
    let err = store
        .list_policy_defs()
        .expect_err("traversal metadata name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());
}
