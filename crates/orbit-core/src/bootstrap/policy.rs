use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_policy_resource;
use orbit_store::contracts::PolicyDefStoreBackend;
use orbit_types::policy::{DEFAULT_POLICY_NAME, PolicyDef};
use orbit_types::resource::ResourceKind;

pub(crate) const DEFAULT_POLICY_FILES: &[(&str, &str)] = &[(
    DEFAULT_POLICY_NAME,
    include_str!("../../assets/policies/default.yaml"),
)];

pub(crate) fn seed_default_policies(
    store: &dyn PolicyDefStoreBackend,
    overwrite: bool,
) -> Result<usize, OrbitError> {
    let now = Utc::now();
    let mut count = 0;
    for (name, raw) in DEFAULT_POLICY_FILES {
        let existing = store.get_policy_def(name)?;
        if existing.is_some() && !overwrite {
            continue;
        }
        let def = parse_default_policy(name, raw, now)?;
        store.upsert_policy_def(&def)?;
        count += 1;
    }
    Ok(count)
}

fn parse_default_policy(
    name: &str,
    raw: &str,
    now: chrono::DateTime<Utc>,
) -> Result<PolicyDef, OrbitError> {
    let resource = parse_policy_resource(raw, &format!("default policy '{name}'"))?;
    if resource.kind != ResourceKind::Policy {
        return Err(OrbitError::InvalidInput(format!(
            "invalid default policy '{name}': expected kind Policy, found {}",
            resource.kind
        )));
    }
    if resource.metadata.name != name {
        return Err(OrbitError::InvalidInput(format!(
            "default policy file key '{}' does not match metadata.name '{}'",
            name, resource.metadata.name
        )));
    }

    let def = PolicyDef {
        name: resource.metadata.name,
        description: resource.spec.description,
        deny_read: resource.spec.deny_read,
        deny_modify: resource.spec.deny_modify,
        fs_profiles: resource.spec.fs_profiles,
        created_at: resource.spec.created_at.or(Some(now)),
        updated_at: resource.spec.updated_at.or(Some(now)),
    };
    def.validate()?;
    Ok(def)
}
