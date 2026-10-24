//! Shipped auto-task defaults a routine replaced, and their retirement.
//!
//! A retired default keeps its last shipped template as provenance, the way
//! a retired routine does: `orbit workspace sync` recognizes a seeded copy
//! by that template's body (operator settings, from the file or the settings
//! table, do not count as an edit) and removes it with its settings entry.
//! An operator-edited copy is not Orbit's, so managed reconciliation
//! preserves it. Sync never enables the replacing routine: when the retired
//! copy was enabled, it says so and names the routine to opt into.

use std::fs;
use std::path::Path;

use orbit_automation::auto_tasks::settings::{load_settings_table, write_settings_table};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_auto_task_yaml;

use crate::application::managed_assets::{
    ConfinedAssetPath, MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout,
    ManagedAssetOutcome, ManagedAssetReconcileMode, load_managed_asset_manifest,
    resolve_confined_asset_path,
};

use super::settings::{AUTO_TASK_ASSET_KIND, classify_bundled_file};

/// A shipped auto-task default this Orbit no longer seeds.
pub(crate) struct RetiredAutoTask {
    /// The definition name, and its file stem.
    pub name: &'static str,
    /// The last template a release shipped for it.
    pub template: &'static str,
    /// The seeded routine (its file stem under `.orbit/routines/`) that
    /// replaces it.
    pub replaced_by: &'static str,
}

/// Retiring a default: drop it from `DEFAULT_AUTO_TASK_FILES`, keep its
/// last shipped template in place and list it here in the same change.
pub(crate) const RETIRED_AUTO_TASK_FILES: &[RetiredAutoTask] = &[RetiredAutoTask {
    name: "blocked-task-triage",
    template: include_str!("../../../assets/auto_tasks/blocked-task-triage.yaml"),
    replaced_by: "blocked_task_triage",
}];

/// What retiring the replaced defaults found in one auto-task directory.
#[derive(Debug, Default)]
pub(crate) struct ReplacedAutoTasks {
    /// One action per seeded copy retired. Managed reconciliation's own
    /// actions for these names are superseded by them.
    pub actions: Vec<ManagedAssetAction>,
    /// Notes on operator-edited copies, which reconciliation preserves.
    pub warnings: Vec<String>,
}

/// Retire every seeded copy of a [`RetiredAutoTask`] in `dir`, and report
/// the routine in `routines_dir` that replaces it. Create-only seeding retires nothing, and
/// `Check` mode reports without writing. An unreadable or unparsable copy,
/// or an unreadable settings table, is left to managed reconciliation and
/// `orbit doctor`. The caller holds the auto-task cursor lock in apply mode.
pub(crate) fn retire_replaced_auto_tasks(
    dir: &Path,
    routines_dir: &Path,
    base_branch: &str,
    mode: ManagedAssetReconcileMode,
) -> Result<ReplacedAutoTasks, OrbitError> {
    let mut replaced = ReplacedAutoTasks::default();
    if mode.creates_only() {
        return Ok(replaced);
    }
    let manifest = load_managed_asset_manifest(
        &dir.join(MANAGED_ASSET_MANIFEST_FILE),
        AUTO_TASK_ASSET_KIND,
        ManagedAssetLayout::YamlStem,
    )?;
    for retired in RETIRED_AUTO_TASK_FILES {
        let relative = ManagedAssetLayout::YamlStem.relative_path(retired.name);
        let ConfinedAssetPath::File(_) = resolve_confined_asset_path(dir, &relative)? else {
            continue;
        };
        let path = dir.join(&relative);
        let on_disk = fs::read_to_string(&path).map_err(|error| {
            OrbitError::io_with_context(&error, format!("read {}: {error}", path.display()))
        })?;
        let Ok(mut table) = load_settings_table(dir) else {
            continue;
        };
        let settings = table.get(retired.name).cloned();
        let Ok(classified) = classify_bundled_file(
            retired.template,
            &on_disk,
            manifest
                .as_ref()
                .and_then(|manifest| manifest.assets.get(retired.name)),
            settings.as_ref(),
            base_branch,
        ) else {
            continue;
        };
        let Ok(mut effective) = parse_auto_task_yaml(&on_disk) else {
            continue;
        };
        if let Some(settings) = &settings {
            settings.apply(&mut effective);
        }
        let routine = ManagedAssetLayout::YamlStem.relative_path(retired.replaced_by);
        let routine = routines_dir.join(routine);
        let routine = routine.display();
        let seeded = classified
            .as_ref()
            .is_none_or(|classified| classified.overrides.body_fields.is_empty());
        if !seeded {
            if effective.enabled {
                replaced.warnings.push(format!(
                    "auto-task `{}` is a locally edited copy of a retired default and was \
                     preserved; the `{}` routine replaces it. Orbit did not enable the \
                     routine: to opt in, set `enabled: true` in {routine}",
                    retired.name, retired.replaced_by
                ));
            }
            continue;
        }
        if mode == ManagedAssetReconcileMode::Apply {
            fs::remove_file(&path).map_err(|error| {
                OrbitError::io_with_context(
                    &error,
                    format!("retire auto-task '{}': {error}", path.display()),
                )
            })?;
            if table.remove(retired.name).is_some() {
                write_settings_table(dir, &table)?;
            }
        }
        replaced.actions.push(ManagedAssetAction {
            name: retired.name.to_string(),
            path,
            outcome: ManagedAssetOutcome::Retired,
            detail: Some(if effective.enabled {
                format!(
                    "the enabled auto-task is replaced by the `{}` routine, which Orbit left \
                     disabled: to opt in, set `enabled: true` in {routine}",
                    retired.replaced_by
                )
            } else {
                format!(
                    "replaced by the `{}` routine, disabled until you opt in ({routine})",
                    retired.replaced_by
                )
            }),
        });
    }
    Ok(replaced)
}
