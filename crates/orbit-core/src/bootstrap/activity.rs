use std::borrow::Cow;
use std::path::Path;

use orbit_common::OrbitError;

use crate::application::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

/// Seed every entry in [`DEFAULT_ACTIVITY_FILES`] as a YAML file under
/// `activities_dir`. Mirrors the skill / executor / policy seeding pattern:
/// the asset YAML is embedded in the binary via `include_str!` and copied
/// out on `orbit init` so the [`V2ActivityCatalog`] can discover it without
/// depending on a git checkout of this repo.
///
/// When `overwrite` is false, existing files are preserved — users who've
/// edited a previously-seeded activity won't lose their changes on re-init.
pub(crate) fn seed_default_activities(
    activities_dir: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        activities_dir,
        "activity",
        ManagedAssetLayout::YamlStem,
        DEFAULT_ACTIVITY_FILES,
        overwrite,
        |_, content| Ok(Cow::Borrowed(content)),
    )
}
