use orbit_core::OrbitError;
use orbit_types::policy::{PolicyDef, UNRESTRICTED_FS_PROFILE};
use serde_json::{Value, json};

pub(super) fn policy_json(def: &PolicyDef) -> Result<Value, OrbitError> {
    Ok(json!({
        "name": def.name,
        "description": def.description,
        "deny_read": def.deny_read,
        "deny_modify": def.deny_modify,
        "fs_profiles": effective_profiles_json(def)?,
        "created_at": def.created_at.map(|t| t.to_rfc3339()),
        "updated_at": def.updated_at.map(|t| t.to_rfc3339()),
    }))
}

pub(super) fn policy_text(def: &PolicyDef) -> Result<String, OrbitError> {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Name:        {}", def.name);
    if let Some(desc) = &def.description {
        let _ = writeln!(out, "Description: {desc}");
    }
    if let Some(created_at) = def.created_at {
        let _ = writeln!(out, "Created:     {}", created_at.to_rfc3339());
    }
    if let Some(updated_at) = def.updated_at {
        let _ = writeln!(out, "Updated:     {}", updated_at.to_rfc3339());
    }

    let _ = writeln!(out, "\nGlobal Denies:");
    let _ = writeln!(out, "  denyRead:   {}", render_rule_list(&def.deny_read));
    let _ = writeln!(out, "  denyModify: {}", render_rule_list(&def.deny_modify));

    let _ = writeln!(out, "\nfsProfiles:");
    for profile_name in sorted_profile_names(def) {
        let effective = def.effective_profile(&profile_name)?;
        let _ = writeln!(out, "  {}:", profile_name);
        let _ = writeln!(out, "    read:   {}", render_rule_list(&effective.read));
        let _ = writeln!(out, "    modify: {}", render_rule_list(&effective.modify));
    }

    Ok(out)
}

fn effective_profiles_json(def: &PolicyDef) -> Result<Value, OrbitError> {
    let mut profiles = Vec::new();
    for profile_name in sorted_profile_names(def) {
        let effective = def.effective_profile(&profile_name)?;
        profiles.push(json!({
            "name": profile_name,
            "read": effective.read,
            "modify": effective.modify,
        }));
    }
    Ok(Value::Array(profiles))
}

pub(super) fn sorted_profile_names(def: &PolicyDef) -> Vec<String> {
    let mut names: Vec<String> = def.fs_profiles.keys().cloned().collect();
    names.sort();
    if !names.iter().any(|name| name == UNRESTRICTED_FS_PROFILE) {
        names.push(UNRESTRICTED_FS_PROFILE.to_string());
    }
    names
}

fn render_rule_list(rules: &[String]) -> String {
    if rules.is_empty() {
        "[]".to_string()
    } else {
        rules.join(", ")
    }
}

pub(super) fn status_word(allowed: bool) -> &'static str {
    if allowed { "allowed" } else { "denied" }
}
