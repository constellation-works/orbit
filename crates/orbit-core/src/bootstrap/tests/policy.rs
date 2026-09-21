//! Shipped default policy evaluation and additive `orbit init` merge.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_policy_resource;
use orbit_store::contracts::PolicyDefStoreBackend;
use orbit_types::policy::{DEFAULT_POLICY_NAME, FsOperation, PolicyDef};
use orbit_types::resource::ResourceKind;

use super::super::policy::{DEFAULT_POLICY_FILES, seed_default_policies};

struct MemoryPolicyStore {
    defs: Mutex<HashMap<String, PolicyDef>>,
}

impl MemoryPolicyStore {
    fn new() -> Self {
        Self {
            defs: Mutex::new(HashMap::new()),
        }
    }
}

impl PolicyDefStoreBackend for MemoryPolicyStore {
    fn list_policy_defs(&self) -> Result<Vec<PolicyDef>, OrbitError> {
        Ok(self
            .defs
            .lock()
            .expect("policy store")
            .values()
            .cloned()
            .collect())
    }

    fn get_policy_def(&self, name: &str) -> Result<Option<PolicyDef>, OrbitError> {
        Ok(self.defs.lock().expect("policy store").get(name).cloned())
    }

    fn upsert_policy_def(&self, def: &PolicyDef) -> Result<(), OrbitError> {
        def.validate()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        self.defs
            .lock()
            .expect("policy store")
            .insert(def.name.clone(), def.clone());
        Ok(())
    }
}

fn shipped_default_policy() -> PolicyDef {
    let (name, raw) = DEFAULT_POLICY_FILES[0];
    let resource = parse_policy_resource(raw, "shipped default policy").expect("parse");
    assert_eq!(resource.kind, ResourceKind::Policy);
    assert_eq!(resource.metadata.name, name);
    let now = Utc::now();
    let def = PolicyDef {
        name: resource.metadata.name,
        description: resource.spec.description,
        deny_read: resource.spec.deny_read,
        deny_modify: resource.spec.deny_modify,
        fs_profiles: resource.spec.fs_profiles,
        created_at: Some(now),
        updated_at: Some(now),
    };
    def.validate()
        .expect("shipped default policy must validate");
    def
}

#[test]
fn shipped_default_policy_allows_scratch_dir_and_denies_orbit_tasks() {
    let def = shipped_default_policy();
    assert!(
        def.deny_modify.iter().any(|rule| rule == "!.orbit/tmp/**"),
        "default.yaml must list !.orbit/tmp/** under denyModify: {:?}",
        def.deny_modify
    );

    let allow = def
        .check_path("implementer", FsOperation::Modify, ".orbit/tmp/x")
        .expect("evaluate scratch path");
    assert!(
        allow.allowed,
        "implementer must be able to modify .orbit/tmp/x: {allow:?}"
    );

    let deny = def
        .check_path("implementer", FsOperation::Modify, ".orbit/tasks/x")
        .expect("evaluate tasks path");
    assert!(
        !deny.allowed,
        "implementer must still be denied .orbit/tasks/x: {deny:?}"
    );
}

#[test]
fn seed_default_policies_merges_missing_scratch_exception_without_clobbering_operator_rules() {
    let store = MemoryPolicyStore::new();
    let mut existing = shipped_default_policy();
    existing.deny_modify.retain(|rule| rule != "!.orbit/tmp/**");
    existing.deny_modify.push("secrets/**".to_string());
    store
        .upsert_policy_def(&existing)
        .expect("seed stale policy");

    let count = seed_default_policies(&store, false).expect("merge");
    assert_eq!(count, 1, "stale default policy must be rewritten once");

    let merged = store
        .get_policy_def(DEFAULT_POLICY_NAME)
        .expect("load")
        .expect("default policy present");
    assert!(
        merged
            .deny_modify
            .iter()
            .any(|rule| rule == "!.orbit/tmp/**"),
        "init merge must add !.orbit/tmp/**: {:?}",
        merged.deny_modify
    );
    assert!(
        merged.deny_modify.iter().any(|rule| rule == "secrets/**"),
        "init merge must keep operator extras: {:?}",
        merged.deny_modify
    );

    let second = seed_default_policies(&store, false).expect("idempotent merge");
    assert_eq!(second, 0, "a current default policy must not be rewritten");
}
