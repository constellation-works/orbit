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
        let shipped = parse_default_policy(name, raw, now)?;
        match store.get_policy_def(name)? {
            Some(existing) if !overwrite => {
                if let Some(merged) = merge_shipped_policy_defaults(existing, &shipped, now) {
                    store.upsert_policy_def(&merged)?;
                    count += 1;
                }
            }
            _ => {
                store.upsert_policy_def(&shipped)?;
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Additive merge of shipped deny rules into an existing default policy.
///
/// Operator-added rules and profile bodies stay in place, matching friction
/// `tags.yaml` merge. Returns `None` when there is nothing to add, or when
/// the merge would not validate (a customized deny list that cannot accept a
/// new exception).
fn merge_shipped_policy_defaults(
    mut existing: PolicyDef,
    shipped: &PolicyDef,
    now: chrono::DateTime<Utc>,
) -> Option<PolicyDef> {
    let deny_read_changed = insert_missing_rules(&mut existing.deny_read, &shipped.deny_read);
    let deny_modify_changed = insert_missing_rules(&mut existing.deny_modify, &shipped.deny_modify);
    if !deny_read_changed && !deny_modify_changed {
        return None;
    }
    existing.updated_at = Some(now);
    existing.validate().ok()?;
    Some(existing)
}

fn insert_missing_rules(existing: &mut Vec<String>, shipped: &[String]) -> bool {
    let mut changed = false;
    let mut cursor = 0usize;
    for rule in shipped {
        if let Some(pos) = existing.iter().position(|item| item == rule) {
            cursor = pos + 1;
            continue;
        }
        existing.insert(cursor, rule.clone());
        cursor += 1;
        changed = true;
    }
    changed
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
