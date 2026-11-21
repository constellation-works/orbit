//! Known shipped bodies, independent of a checkout's last-written manifest.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use orbit_automation::auto_tasks::settings::{
    AutoTaskOverrides, AutoTaskSettings, split_overrides,
};
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::normalize_required_tools;
use orbit_types::tool::pre_family_tool_name;
use orbit_types::workflow::AutoTaskDefinition;
use serde::Deserialize;

use super::BASE_BRANCH_PLACEHOLDER;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedBody {
    digest: String,
    #[serde(rename = "revision")]
    _revision: String,
    settings: AutoTaskSettings,
    comments: Vec<String>,
}

type BodyHistory = BTreeMap<String, Vec<ShippedBody>>;

fn history() -> Result<&'static BodyHistory, OrbitError> {
    static HISTORY: OnceLock<Result<BodyHistory, String>> = OnceLock::new();
    HISTORY
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../assets/auto_tasks/body-history.json"))
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| {
            OrbitError::InvalidInput(format!("bundled auto-task body history: {error}"))
        })
}

fn body_digest(definition: &AutoTaskDefinition, base_branch: &str) -> Result<String, OrbitError> {
    let mut body = serde_json::to_value(definition)
        .map_err(|error| OrbitError::InvalidInput(format!("auto-task body digest: {error}")))?;
    if let Some(fields) = body.as_object_mut() {
        for field in [
            "enabled",
            "schedule",
            "dedupe",
            "granted_tools",
            "created_by",
            "created_at",
            "updated_by",
            "updated_at",
        ] {
            fields.remove(field);
        }
    }
    if let Some(template) = body
        .get_mut("template")
        .and_then(serde_json::Value::as_object_mut)
    {
        for field in ["crew", "priority", "complexity", "tags"] {
            template.remove(field);
        }
    }
    if let Some(reference) = body.pointer_mut("/skip_if_unchanged/ref")
        && reference.as_str().is_some_and(|reference| {
            reference == base_branch
                || reference == "agent-main"
                || reference == BASE_BRANCH_PLACEHOLDER
        })
    {
        *reference = BASE_BRANCH_PLACEHOLDER.into();
    }
    body.sort_all_objects();
    let canonical = serde_json::to_vec(&body)
        .map_err(|error| OrbitError::InvalidInput(format!("auto-task body digest: {error}")))?;
    Ok(sha256_hex(&canonical))
}

/// Recognize a previous shipped body plus settings, preserving added comments,
/// removed body tags, and cleared crew/complexity as body edits. A setting
/// still holding its historical shipped value is an upstream default, not an
/// operator's override, so it never masks today's default on migration.
pub(super) fn historical_overrides(
    current: &AutoTaskDefinition,
    definition: &AutoTaskDefinition,
    raw: &str,
    base_branch: &str,
    settings: Option<&AutoTaskSettings>,
) -> Result<Option<AutoTaskOverrides>, OrbitError> {
    let Some(versions) = history()?.get(&definition.name) else {
        return Ok(None);
    };
    let digest = body_digest(definition, base_branch)?;
    // A body shipped before tool families was fingerprinted with operation
    // names; a definition rewritten to family actions [ORB-23552] still
    // matches it under the names it was shipped with.
    let pre_family_digest = {
        let mut pre_family = definition.clone();
        pre_family.template.required_tools = normalize_required_tools(
            pre_family
                .template
                .required_tools
                .iter()
                .map(|name| pre_family_tool_name(name).to_string())
                .collect(),
        );
        body_digest(&pre_family, base_branch)?
    };
    // Several releases can ship the same body with different settings
    // (host-health shipped that body enabled, then disabled). The version
    // whose settings the file matches most closely is the one it was copied
    // from; settings equal to that version's shipped values are upstream
    // defaults, not operator overrides, so they must not mask today's
    // default when the body is migrated.
    let mut best: Option<(usize, AutoTaskOverrides)> = None;
    for version in versions {
        if (version.digest != digest && version.digest != pre_family_digest)
            || raw.lines().map(str::trim).any(|line| {
                line.starts_with('#') && !version.comments.iter().any(|comment| comment == line)
            })
        {
            continue;
        }
        let mut historical = definition.clone();
        let mut defaults = version.settings.clone();
        // Schedules are settings, but historical branch placeholders still
        // need the same rendering as a currently bundled definition.
        if let Some(orbit_types::workflow::AutoTaskSchedule::Deliveries {
            deliveries_landed, ..
        }) = defaults.schedule.as_mut()
            && deliveries_landed.branch == BASE_BRANCH_PLACEHOLDER
        {
            deliveries_landed.branch = base_branch.to_string();
        }
        defaults.apply(&mut historical);
        historical.template.crew = defaults.crew.clone();
        historical.template.complexity = defaults.complexity;
        historical.template.tags = defaults.tags.clone();
        historical.granted_tools = defaults.granted_tools.clone().unwrap_or_default();
        let historical_diff = split_overrides(&historical, definition);
        if !historical_diff.body_fields.is_empty() {
            continue;
        }
        let distance =
            historical_diff.settings.field_names().len() + historical_diff.settings.tags.len();
        let mut effective = definition.clone();
        if let Some(settings) = settings {
            settings.apply(&mut effective);
        }
        let mut overrides = split_overrides(current, &effective);
        // The historical comparison above proves these differences are all
        // upstream body changes, rather than local edits.
        overrides.body_fields.clear();
        // A setting that still holds the historical shipped value is an
        // upstream default that has since changed, not an operator choice:
        // drop it so the migrated body takes today's default. Settings an
        // operator changed away from that shipped value differ from it and
        // are preserved below.
        if overrides.settings.enabled == defaults.enabled {
            overrides.settings.enabled = None;
        }
        if overrides.settings.schedule == defaults.schedule {
            overrides.settings.schedule = None;
        }
        if overrides.settings.dedupe == defaults.dedupe {
            overrides.settings.dedupe = None;
        }
        if overrides.settings.crew == defaults.crew {
            overrides.settings.crew = None;
        }
        if overrides.settings.priority == defaults.priority {
            overrides.settings.priority = None;
        }
        if overrides.settings.complexity == defaults.complexity {
            overrides.settings.complexity = None;
        }
        overrides
            .settings
            .tags
            .retain(|tag| !defaults.tags.contains(tag));
        if overrides
            .settings
            .granted_tools
            .as_ref()
            .is_some_and(|tools| *tools == historical.granted_tools)
        {
            overrides.settings.granted_tools = None;
        }
        // [ORB-15663] Shipped bodies once named the retired `system` crew;
        // that default is an upstream change too, so the template now routes
        // through its complexity pool. A settings-table crew still wins below.
        if overrides.settings.crew.as_deref() == Some(orbit_config::RETIRED_SYSTEM_CREW) {
            overrides.settings.crew = None;
        }
        // Keep explicit table entries even if they equal today's default.
        if let Some(settings) = settings {
            overrides.settings.enabled = settings.enabled.or(overrides.settings.enabled);
            overrides.settings.schedule = settings.schedule.clone().or(overrides.settings.schedule);
            overrides.settings.dedupe = settings.dedupe.or(overrides.settings.dedupe);
            overrides.settings.crew = settings.crew.clone().or(overrides.settings.crew);
            overrides.settings.priority = settings.priority.or(overrides.settings.priority);
            overrides.settings.complexity = settings.complexity.or(overrides.settings.complexity);
            overrides.settings.granted_tools = settings
                .granted_tools
                .clone()
                .or(overrides.settings.granted_tools);
            for tag in &settings.tags {
                if !overrides.settings.tags.contains(tag) {
                    overrides.settings.tags.push(tag.clone());
                }
            }
        }
        if best
            .as_ref()
            .is_none_or(|(best_distance, _)| distance < *best_distance)
        {
            best = Some((distance, overrides));
        }
        if distance == 0 {
            break;
        }
    }
    Ok(best.map(|(_, overrides)| overrides))
}
