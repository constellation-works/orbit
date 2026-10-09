//! Managed-asset reconciliation: materialize embedded default catalogs and
//! retire the ones a later release dropped, by content provenance recorded
//! in a per-directory manifest.

mod manifest;
mod reconcile;
mod retired;

pub(crate) use manifest::{
    ConfinedAssetPath, record_managed_manifest_write, resolve_confined_asset_path,
};
pub(super) use manifest::{
    encode_managed_asset_manifest, load_managed_asset_manifest, write_managed_asset_manifest,
};
pub(crate) use reconcile::{
    ManagedAssetOptOut, reconcile_managed_assets, reconcile_managed_assets_in_mode,
    record_managed_asset_opt_out, restore_managed_asset,
};
pub(super) use retired::preserve_modified_retired_asset;
pub(crate) use retired::retired_preservation_path;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const MANAGED_ASSET_MANIFEST_FILE: &str = ".orbit-managed-assets.json";
const MANAGED_ASSET_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub(super) const ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ManagedAssetReconciliation {
    pub refreshed: usize,
    pub retired: usize,
    pub warnings: Vec<String>,
    pub actions: Vec<ManagedAssetAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedAssetReconcileMode {
    /// Converge the directory: create, refresh, and retire by provenance.
    Apply,
    /// Create only the shipped defaults that are absent. Files that exist,
    /// their recorded provenance, and retirements are left exactly as they
    /// are, so an operator's edits and user-authored files are never touched.
    CreateAbsent,
    /// Report what `Apply` would do without writing anything.
    Check,
}

impl ManagedAssetReconcileMode {
    /// Whether this mode may create or rewrite files and the manifest.
    pub(crate) fn writes(self) -> bool {
        !matches!(self, Self::Check)
    }

    /// Whether this mode leaves files that already exist, and retirements,
    /// alone.
    pub(crate) fn creates_only(self) -> bool {
        matches!(self, Self::CreateAbsent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedAssetOutcome {
    Created,
    Refreshed,
    Retired,
    Migrated,
    Preserved,
    BindingDrift,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAssetAction {
    pub name: String,
    pub path: PathBuf,
    pub outcome: ManagedAssetOutcome,
    pub detail: Option<String>,
}

/// How a manifest key maps to the file it manages, relative to the managed
/// directory.
///
/// Four of the five artifact kinds are flat single-document catalogs whose
/// manifest key is the definition name ([`ManagedAssetLayout::YamlStem`]).
/// Skills are directory trees — one `SKILL.md` plus optional reference files
/// per skill id — so their manifest keys are the relative paths themselves
/// ([`ManagedAssetLayout::RelativePath`]).
// ADR-0366 extends ADR-0346's provenance mechanism to tree-shaped assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedAssetLayout {
    /// `<name>.yaml` — activities, jobs, auto-tasks, routines.
    YamlStem,
    /// `<name>` verbatim, a `/`-separated relative path — skills.
    RelativePath,
}

impl ManagedAssetLayout {
    /// Resolve one manifest key to its path relative to the managed directory.
    pub(super) fn relative_path(self, name: &str) -> PathBuf {
        match self {
            Self::YamlStem => PathBuf::from(format!("{name}.yaml")),
            Self::RelativePath => PathBuf::from(name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ManagedAssetManifest {
    pub(super) schema_version: u32,
    pub(super) asset_kind: String,
    pub(super) assets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) routine_provenance: BTreeMap<String, RoutineAssetProvenance>,
    /// Shipped defaults an operator deleted. Reconciliation leaves them
    /// absent instead of re-creating them, and doctor does not report them
    /// missing. A name drops out once the binary stops shipping it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) opted_out: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RoutineAssetProvenance {
    pub template_digest: String,
    pub rendered_digest: String,
    pub binding: RoutineMaterializationBinding,
}

/// The per-workspace values a shipped routine template is rendered against.
///
/// Templates no longer render a host pin [ORB-12236]; the binding is the
/// routine name plus, only for a template that declares a state trigger, the
/// owner machine and observed branch that trigger requires [ORB-12745]. A
/// manifest written before the host pin was retired still carries a `hosts`
/// entry, which is why `deny_unknown_fields` is off here: the stale entry
/// loads and its routine reconciles as an ordinary managed refresh. Restore
/// `deny_unknown_fields` after 2026-12-01.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoutineMaterializationBinding {
    pub name: String,
    /// The registered machine id a state trigger names as its owner; `None`
    /// for a template without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_machine: Option<String>,
    /// The branch a state trigger observes; `None` for a template without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}
