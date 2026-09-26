//! Standing health of the *definition artifacts* a workspace accumulates —
//! skills, jobs, activities, auto-tasks, and routines [ORB-10800].
//!
//! `orbit doctor` already diagnoses infrastructure (config, database, disk,
//! indexes, locks, runs). This module supplies the missing half: the
//! definitions themselves, classified into four conditions.
//!
//! - **Faulty** — the file fails to parse or validate, so its definition is
//!   absent at dispatch time even though the file is still on disk.
//! - **Residual** — an on-disk skill directory has no `SKILL.md` entry point,
//!   so it cannot load but still occupies the catalog.
//! - **Deprecated** — the managed manifest proves Orbit wrote this file for a
//!   default that the running binary no longer ships. A routine is also
//!   deprecated when its content matches a retired template and the manifest
//!   never recorded it: reconciliation retires that file by content, so
//!   reporting the catalog healthy would contradict it [DANI-10502].
//! - **Stale** — the file is a managed copy of an *older* release of a default
//!   this binary still ships, or an untracked file colliding with a bundled
//!   default name.
//! - **Missing** — the managed catalog was previously reconciled, and a primary
//!   shipped default this binary still embeds is absent from disk. Warm opens
//!   skip reconciliation when the defaults stamp matches, so this state can
//!   persist until `orbit init` or `orbit workspace sync` restores the file.
//!   A default the manifest records as operator-deleted (`orbit auto-task
//!   delete`) is an opt-out, reported neither missing nor stale.
//!
//! Provenance judgements are made from the per-kind managed manifest written by
//! [`crate::application::managed_assets::reconcile_managed_assets`]. Residual
//! skill directories are the one condition discovered directly from the catalog layout because a deleted
//! entry point cannot be represented by a successfully loaded skill. That
//! matters for correctness as well as safety: precedence differs across kinds —
//! skills merge workspace-over-global while activities keep shipped defaults
//! authoritative over workspace copies — so a rule phrased in terms of "which
//! copy wins" would misreport at least one kind. Provenance is a property of
//! the file Orbit wrote, in the directory Orbit wrote it to, and is unaffected
//! by which copy a loader later prefers.
//!
//! Repair ([`OrbitRuntime::remove_stale_definition_artifacts`]) is deliberately
//! narrower than diagnosis: only a *deprecated* artifact whose digest still
//! proves Orbit wrote it is removed. A locally modified one is preserved
//! outside the active catalog exactly as init-time reconciliation does, and a
//! faulty user-authored file is never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::activity_catalog::repair_retired_activity_backends as repair_activity_backends;

pub use super::activity_catalog::{
    FIX_RETIRED_ACTIVITY_BACKENDS_CMD, RetiredActivityBackendRepair, RetiredActivityBackendSkip,
};

use super::artifact_diagnosis::diagnose_catalog;
use crate::OrbitRuntime;
use crate::application::auto_tasks::auto_tasks_dir;

use crate::application::auto_tasks::{DEFAULT_AUTO_TASK_FILES, render_default_auto_task};
use crate::application::job::catalog::DEFAULT_JOB_FILES;
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, load_managed_asset_manifest,
    preserve_modified_retired_asset,
};
use crate::application::routines::seed::DEFAULT_ROUTINE_FILES;
use crate::application::skill::{DEFAULT_SKILL_FILES, inject_skill_template_tokens};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;
use orbit_common::security::release::sha256_hex;

/// The five definition-artifact kinds Orbit ships defaults for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Skill,
    Job,
    Activity,
    AutoTask,
    Routine,
}

impl ArtifactKind {
    /// Stable identifier used in doctor check names and operator messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Job => "jobs",
            Self::Activity => "activities",
            Self::AutoTask => "auto-tasks",
            Self::Routine => "routines",
        }
    }

    /// Singular noun for a single artifact of this kind.
    pub fn singular(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Job => "job",
            Self::Activity => "activity",
            Self::AutoTask => "auto-task",
            Self::Routine => "routine",
        }
    }

    /// The `assetKind` recorded in this kind's managed manifest.
    pub(super) fn asset_kind(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Job => "job",
            Self::Activity => "activity",
            Self::AutoTask => "auto_task",
            Self::Routine => "routine",
        }
    }

    pub(super) fn layout(self) -> ManagedAssetLayout {
        match self {
            Self::Skill => ManagedAssetLayout::RelativePath,
            _ => ManagedAssetLayout::YamlStem,
        }
    }
}

/// Command that refreshes the catalog containing this artifact kind.
pub(super) fn init_command(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Skill | ArtifactKind::Job | ArtifactKind::Activity => "orbit init",
        ArtifactKind::AutoTask | ArtifactKind::Routine => "orbit workspace sync",
    }
}

/// Why an artifact is not healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCondition {
    /// Fails to parse or validate — absent at dispatch time.
    Faulty,
    /// A skill directory remains on disk without its `SKILL.md` entry point.
    Residual,
    /// A managed default this binary no longer ships.
    Deprecated,
    /// Drifted from the current release, or colliding with a bundled name.
    Stale,
    /// A primary shipped default this binary still embeds is not on disk.
    Missing,
}

impl ArtifactCondition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Faulty => "faulty",
            Self::Residual => "residual",
            Self::Deprecated => "deprecated",
            Self::Stale => "stale",
            Self::Missing => "missing",
        }
    }
}

/// What the managed manifest proves about who wrote an artifact's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactProvenance {
    /// The recorded digest matches the file: Orbit wrote exactly this content.
    OrbitWritten,
    /// Tracked by the manifest, but edited since Orbit wrote it.
    LocallyModified,
    /// No managed provenance at all — authored in this workspace.
    UserAuthored,
}

impl ArtifactProvenance {
    /// Whether a deprecated finding's detail may describe this artifact as
    /// Orbit's own, unmodified content. `OrbitWritten` also covers a routine
    /// recognized by shape rather than by digest (an operator's lifecycle
    /// edit), so this alone does not license `--fix-stale-artifacts` to
    /// delete the file outright: `retire_catalog` independently re-checks
    /// the digest and deletes only a byte-exact match, preserving a copy
    /// under `.retired-managed/` for anything else — exactly as `workspace
    /// sync` does.
    pub(super) fn is_removable(self) -> bool {
        matches!(self, Self::OrbitWritten)
    }
}

/// One unhealthy artifact.
#[derive(Debug, Clone)]
pub struct ArtifactFinding {
    pub kind: ArtifactKind,
    /// Manifest key / definition name.
    pub name: String,
    pub path: PathBuf,
    pub condition: ArtifactCondition,
    pub provenance: ArtifactProvenance,
    /// Human-readable specifics.
    pub detail: String,
    /// The exact repair command or manual step for this finding.
    pub remediation: String,
}

impl ArtifactFinding {
    /// A shipped default that no longer loads is a broken install rather than
    /// a workspace authoring mistake, and is the only artifact fault that
    /// escalates `orbit doctor` to a nonzero exit. That includes a managed
    /// file that is simply gone: dispatch cannot load what is not on disk.
    pub fn is_unloadable_shipped_default(&self) -> bool {
        match self.condition {
            ArtifactCondition::Faulty => self.provenance != ArtifactProvenance::UserAuthored,
            ArtifactCondition::Missing => true,
            ArtifactCondition::Residual
            | ArtifactCondition::Deprecated
            | ArtifactCondition::Stale => false,
        }
    }
}

/// Per-kind diagnosis: what was inspected and what came back unhealthy.
#[derive(Debug, Clone)]
pub struct ArtifactHealth {
    pub kind: ArtifactKind,
    /// Artifact catalog entries inspected for this kind.
    pub scanned: usize,
    /// Unhealthy artifacts, deterministically ordered.
    pub findings: Vec<ArtifactFinding>,
}

/// One kind's managed directory plus the assets this binary currently ships
/// for it.
pub(super) struct ManagedCatalog {
    pub(super) kind: ArtifactKind,
    pub(super) dir: PathBuf,
    /// Manifest key → rendered content, when this binary can reproduce what it
    /// would write here. `None` for routines: their rendered form pins a host
    /// identity that higher-level composition owns and that core deliberately
    /// never resolves on its own, so content drift is not decidable here (name
    /// retirement and collisions still are).
    pub(super) embedded: Option<BTreeMap<String, String>>,
    /// Manifest keys this binary ships, always known.
    pub(super) shipped: BTreeSet<String>,
}

impl ManagedCatalog {
    fn rendered(
        kind: ArtifactKind,
        dir: PathBuf,
        assets: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let embedded: BTreeMap<String, String> = assets.into_iter().collect();
        let shipped = embedded.keys().cloned().collect();
        Self {
            kind,
            dir,
            embedded: Some(embedded),
            shipped,
        }
    }

    fn names_only(
        kind: ArtifactKind,
        dir: PathBuf,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            kind,
            dir,
            embedded: None,
            shipped: names.into_iter().collect(),
        }
    }

    pub(super) fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(self.kind.layout().relative_path(name))
    }
}

/// Every managed catalog for this runtime, in doctor display order.
fn managed_catalogs(runtime: &OrbitRuntime) -> Vec<ManagedCatalog> {
    let global_root = runtime.global_root();
    let local_dir = runtime.paths().local_dir.clone();
    let owned = |assets: &[(&str, &str)]| -> Vec<(String, String)> {
        assets
            .iter()
            .map(|(name, content)| ((*name).to_string(), (*content).to_string()))
            .collect()
    };

    vec![
        ManagedCatalog::rendered(
            ArtifactKind::Skill,
            global_root.join("skills"),
            DEFAULT_SKILL_FILES.iter().map(|(name, content)| {
                (
                    (*name).to_string(),
                    inject_skill_template_tokens(content, &global_root),
                )
            }),
        ),
        ManagedCatalog::rendered(
            ArtifactKind::Job,
            global_root.join("resources/jobs"),
            owned(DEFAULT_JOB_FILES),
        ),
        ManagedCatalog::rendered(
            ArtifactKind::Activity,
            global_root.join("resources/activities"),
            owned(DEFAULT_ACTIVITY_FILES),
        ),
        ManagedCatalog::rendered(
            ArtifactKind::AutoTask,
            auto_tasks_dir(&local_dir),
            DEFAULT_AUTO_TASK_FILES.iter().map(|(name, content)| {
                (
                    (*name).to_string(),
                    render_default_auto_task(content, runtime.workspace_base_branch()).into_owned(),
                )
            }),
        ),
        ManagedCatalog::names_only(
            ArtifactKind::Routine,
            local_dir.join("routines"),
            DEFAULT_ROUTINE_FILES
                .iter()
                .map(|(name, _)| (*name).to_string()),
        ),
    ]
}

impl OrbitRuntime {
    /// Diagnose every definition-artifact kind. Probe failures degrade into
    /// findings rather than aborting the pass, mirroring `orbit doctor`'s
    /// contract that one broken subsystem never hides the rest.
    pub fn inspect_definition_artifacts(&self) -> Result<Vec<ArtifactHealth>, OrbitError> {
        let mut report = Vec::new();
        for catalog in managed_catalogs(self) {
            report.push(diagnose_catalog(self, &catalog));
        }
        Ok(report)
    }

    /// Retire deprecated managed artifacts: delete the ones whose recorded
    /// digest proves Orbit wrote them, preserve locally modified ones outside
    /// the active catalog, and leave everything else — faulty, stale, and
    /// user-authored files alike — exactly as found.
    ///
    /// Returns the number of artifacts removed from the active catalog.
    pub fn remove_stale_definition_artifacts(&self) -> Result<usize, OrbitError> {
        let mut removed = 0usize;
        for catalog in managed_catalogs(self) {
            removed += retire_catalog(&catalog)?;
        }
        Ok(removed)
    }

    /// Remove known retired `spec.backend` values from schemaVersion 2
    /// agent-loop activities. Unknown backends and unrelated malformed
    /// files are left untouched and listed for a manual edit.
    pub fn repair_retired_activity_backends(
        &self,
    ) -> Result<RetiredActivityBackendRepair, OrbitError> {
        repair_activity_backends(self)
    }
}

/// Read one artifact file, treating an unreadable file as absent.
pub(super) fn read_artifact(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Classify a file's provenance against the manifest digest recorded for it.
///
/// Routines get the same shape-aware answer `orbit workspace sync` gives: an
/// operator's `enabled` opt-in or a dropped `hosts:` key on a template Orbit
/// shipped is still Orbit-written, so the two surfaces never disagree about
/// whether a retired default is safe to delete.
pub(super) fn provenance(
    kind: ArtifactKind,
    name: &str,
    tracked: Option<&String>,
    on_disk: &str,
) -> ArtifactProvenance {
    match tracked {
        Some(digest) if *digest == sha256_hex(on_disk.as_bytes()) => {
            ArtifactProvenance::OrbitWritten
        }
        Some(digest)
            if kind == ArtifactKind::Routine
                && crate::application::routines::template::is_orbit_written_routine(
                    name, digest, on_disk,
                ) =>
        {
            ArtifactProvenance::OrbitWritten
        }
        Some(_) => ArtifactProvenance::LocallyModified,
        None => ArtifactProvenance::UserAuthored,
    }
}

/// Retire one catalog's deprecated managed artifacts and drop them from its
/// manifest. Returns how many left the active catalog.
fn retire_catalog(catalog: &ManagedCatalog) -> Result<usize, OrbitError> {
    let kind = catalog.kind;
    let manifest_path = catalog.dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let Some(manifest) =
        load_managed_asset_manifest(&manifest_path, kind.asset_kind(), kind.layout())?
    else {
        return Ok(0);
    };

    let mut retired = 0usize;
    let mut settled = Vec::new();
    for (name, digest) in &manifest.assets {
        if catalog.shipped.contains(name) {
            continue;
        }
        let relative = kind.layout().relative_path(name);
        let Some(on_disk) = read_artifact(&catalog.dir.join(&relative)) else {
            // Already gone: drop the manifest entry so the next pass is clean.
            settled.push(name.clone());
            continue;
        };
        let path = match resolve_removable_artifact(&catalog.dir, &relative)? {
            Some(path) => path,
            // A symlinked artifact is a deliberate operator arrangement;
            // removing it here would act on a target outside this catalog.
            None => continue,
        };
        // Deletion outright requires an exact digest match, the same
        // `byte_exact` test `reconcile_default_routines` applies: a routine
        // recognized as Orbit-written by shape rather than by digest (an
        // operator's lifecycle edit) is not byte-for-byte what Orbit wrote,
        // so it is preserved here exactly as `workspace sync` preserves it —
        // `is_removable()` alone does not license the delete.
        if sha256_hex(on_disk.as_bytes()) == *digest {
            std::fs::remove_file(&path).map_err(|error| {
                OrbitError::Io(format!(
                    "retire deprecated {} '{}': {error}",
                    kind.singular(),
                    path.display()
                ))
            })?;
        } else {
            let preserved = preserve_modified_retired_asset(
                &catalog.dir,
                kind.asset_kind(),
                kind.layout(),
                name,
                &path,
            )?;
            tracing::warn!(
                target: "orbit.core.artifact_health",
                artifact_kind = kind.singular(),
                artifact = name.as_str(),
                preserved = %preserved.display(),
                "deprecated artifact differs from the bytes Orbit wrote and was preserved outside the active catalog"
            );
        }
        settled.push(name.clone());
        retired += 1;
    }

    if !settled.is_empty() {
        let mut next = manifest.clone();
        for name in &settled {
            next.assets.remove(name);
        }
        crate::application::managed_assets::write_managed_asset_manifest(&manifest_path, &next)?;
    }
    Ok(retired)
}

/// Resolve a managed artifact for removal, refusing anything that could act
/// outside `dir`.
///
/// The relative path is re-validated even though it came from a manifest that
/// validated it on load, and the final component is inspected with
/// `symlink_metadata` so removal never follows a symlink at the boundary —
/// mirroring `remove_workspace_subtree` in the doctor's lock cleanup.
fn resolve_removable_artifact(dir: &Path, relative: &Path) -> Result<Option<PathBuf>, OrbitError> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(OrbitError::InvalidInput(format!(
            "managed artifact path '{}' must remain relative to '{}'",
            relative.display(),
            dir.display()
        )));
    }
    let target = dir.join(relative);
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect managed artifact {}: {error}",
                target.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || metadata.is_dir() {
        return Ok(None);
    }
    Ok(Some(target))
}
