//! Shipped auto-task defaults a release demoted to workspace-local.
//!
//! A demoted default ([`orbit_config::DEMOTED_DEFAULT_AUTO_TASKS`]) left the
//! shipped set because it had not proven its yield, not because something
//! replaced it. Unlike a retired default, a workspace's existing copy is
//! kept: `orbit workspace sync` drops only its managed-manifest provenance,
//! so the file stays in place as a workspace-local definition, with its
//! operator settings, schedule and `enabled` state untouched. Managed
//! reconciliation would otherwise retire it as a dropped default, and doctor
//! skips it for the same reason.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_config::DEMOTED_DEFAULT_AUTO_TASKS;

use crate::application::managed_assets::{
    ConfinedAssetPath, MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout,
    ManagedAssetOutcome, ManagedAssetReconcileMode, encode_managed_asset_manifest,
    load_managed_asset_manifest, record_managed_manifest_write, resolve_confined_asset_path,
};

use super::settings::AUTO_TASK_ASSET_KIND;

/// What releasing the demoted defaults did in one auto-task directory.
#[derive(Debug, Default)]
pub(crate) struct ReleasedAutoTasks {
    /// One action per demoted copy whose provenance was dropped. Managed
    /// reconciliation's own actions for these names are superseded by them.
    pub actions: Vec<ManagedAssetAction>,
    /// A manifest write skipped on a read-only catalog.
    pub warnings: Vec<String>,
}

/// Release every demoted default `dir`'s manifest still tracks: forget its
/// recorded digest so reconciliation leaves the file alone and later passes
/// treat it as user-authored. The file itself, its settings entry and its
/// cursor are not touched. Create-only seeding releases nothing (it retires
/// nothing either), and `Check` mode reports without writing. The caller
/// holds the auto-task cursor lock in apply mode.
pub(crate) fn release_demoted_auto_tasks(
    dir: &Path,
    mode: ManagedAssetReconcileMode,
) -> Result<ReleasedAutoTasks, OrbitError> {
    let mut released = ReleasedAutoTasks::default();
    if mode.creates_only() {
        return Ok(released);
    }
    let manifest_path = dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let Some(mut manifest) = load_managed_asset_manifest(
        &manifest_path,
        AUTO_TASK_ASSET_KIND,
        ManagedAssetLayout::YamlStem,
    )?
    else {
        return Ok(released);
    };
    for name in DEMOTED_DEFAULT_AUTO_TASKS {
        if manifest.assets.remove(*name).is_none() {
            continue;
        }
        let relative = ManagedAssetLayout::YamlStem.relative_path(name);
        let detail = match resolve_confined_asset_path(dir, &relative)? {
            ConfinedAssetPath::File(_) => format!(
                "`{name}` is no longer a shipped default; the existing copy is kept as a \
                 workspace-local definition"
            ),
            ConfinedAssetPath::Missing => {
                "removed stale manifest provenance for an absent artifact".to_string()
            }
            // Nothing is read or written through the link: only the
            // manifest entry goes, so no later pass retires through it.
            ConfinedAssetPath::Unsafe(_) => format!(
                "`{name}` is no longer a shipped default; its manifest provenance was dropped \
                 and the linked path left untouched"
            ),
        };
        released.actions.push(ManagedAssetAction {
            name: (*name).to_string(),
            path: dir.join(&relative),
            outcome: ManagedAssetOutcome::Migrated,
            detail: Some(detail),
        });
    }
    if !released.actions.is_empty() && mode == ManagedAssetReconcileMode::Apply {
        let encoded = encode_managed_asset_manifest(&manifest)?;
        record_managed_manifest_write(
            &manifest_path,
            AUTO_TASK_ASSET_KIND,
            atomic_write_text(&manifest_path, &encoded),
            &mut released.warnings,
        )?;
    }
    Ok(released)
}
