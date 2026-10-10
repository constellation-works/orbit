//! Known shipped bodies, independent of a checkout's last-written manifest.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use orbit_automation::auto_tasks::settings::{
    AutoTaskOverrides, AutoTaskSettings, split_overrides,
};
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
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
/// removed body tags, and cleared crew/complexity as body edits. Settings are
/// compared with today's default so a known older setting never masks an
/// operator's override when multiple releases shipped the same body.
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
    for version in versions {
        if version.digest != digest
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
        if let Some(orbit_types::workflow::AutoTaskSchedule::Deliveries { deliveries_landed }) =
            defaults.schedule.as_mut()
            && deliveries_landed.branch == BASE_BRANCH_PLACEHOLDER
        {
            deliveries_landed.branch = base_branch.to_string();
        }
        defaults.apply(&mut historical);
        historical.template.crew = defaults.crew;
        historical.template.complexity = defaults.complexity;
        historical.template.tags = defaults.tags;
        if !split_overrides(&historical, definition)
            .body_fields
            .is_empty()
        {
            continue;
        }
        let mut effective = definition.clone();
        if let Some(settings) = settings {
            settings.apply(&mut effective);
        }
        let mut overrides = split_overrides(current, &effective);
        // The historical comparison above proves these differences are all
        // upstream body changes, rather than local edits.
        overrides.body_fields.clear();
        // Keep explicit table entries even if they equal today's default.
        if let Some(settings) = settings {
            overrides.settings.enabled = settings.enabled.or(overrides.settings.enabled);
            overrides.settings.schedule = settings.schedule.clone().or(overrides.settings.schedule);
            overrides.settings.dedupe = settings.dedupe.or(overrides.settings.dedupe);
            overrides.settings.crew = settings.crew.clone().or(overrides.settings.crew);
            overrides.settings.priority = settings.priority.or(overrides.settings.priority);
            overrides.settings.complexity = settings.complexity.or(overrides.settings.complexity);
            for tag in &settings.tags {
                if !overrides.settings.tags.contains(tag) {
                    overrides.settings.tags.push(tag.clone());
                }
            }
        }
        return Ok(Some(overrides));
    }
    Ok(None)
}
