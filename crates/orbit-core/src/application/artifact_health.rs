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
//!   default that the running binary no longer ships.
//! - **Stale** — the file is a managed copy of an *older* release of a default
//!   this binary still ships, or an untracked file colliding with a bundled
//!   default name.
//! - **Missing** — the managed catalog was previously reconciled, and a primary
//!   shipped default this binary still embeds is absent from disk. Warm opens
//!   skip reconciliation when the defaults stamp matches, so this state can
//!   persist until `orbit init` or `orbit workspace sync` restores the file.
//!
//! Provenance judgements are made from the per-kind managed manifest written by
//! [`super::reconcile_managed_assets`]. Residual skill directories are the one
//! condition discovered directly from the catalog layout because a deleted
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
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_engine::activity_job::load_job_asset;

use super::activity_catalog_health::{
    ActivityCatalogFault, collect_activity_catalog_faults,
    repair_retired_activity_backends as repair_activity_backends,
};

pub use super::activity_catalog_health::{
    FIX_RETIRED_ACTIVITY_BACKENDS_CMD, RetiredActivityBackendRepair, RetiredActivityBackendSkip,
};

use crate::OrbitRuntime;
use crate::application::auto_tasks::{auto_tasks_dir, collect_auto_tasks};

use super::job::catalog::DEFAULT_JOB_FILES;
use super::skill::{DEFAULT_SKILL_FILES, inject_skill_template_tokens};
use super::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, load_managed_asset_manifest,
    preserve_modified_retired_asset, sha256_hex,
};
use crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES;
use crate::application::routine::DEFAULT_ROUTINE_FILES;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

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
    fn asset_kind(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Job => "job",
            Self::Activity => "activity",
            Self::AutoTask => "auto_task",
            Self::Routine => "routine",
        }
    }

    fn layout(self) -> ManagedAssetLayout {
        match self {
            Self::Skill => ManagedAssetLayout::RelativePath,
            _ => ManagedAssetLayout::YamlStem,
        }
    }
}

/// Command that refreshes the catalog containing this artifact kind.
fn init_command(kind: ArtifactKind) -> &'static str {
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
    /// Whether `--fix-stale-artifacts` may delete this file outright. Only a
    /// digest match proves Orbit wrote it and that no local edit is at risk.
    fn is_removable(self) -> bool {
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
struct ManagedCatalog {
    kind: ArtifactKind,
    dir: PathBuf,
    /// Manifest key → rendered content, when this binary can reproduce what it
    /// would write here. `None` for routines: their rendered form pins a host
    /// identity that higher-level composition owns and that core deliberately
    /// never resolves on its own, so content drift is not decidable here (name
    /// retirement and collisions still are).
    embedded: Option<BTreeMap<String, String>>,
    /// Manifest keys this binary ships, always known.
    shipped: BTreeSet<String>,
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

    fn path_of(&self, name: &str) -> PathBuf {
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
            owned(DEFAULT_AUTO_TASK_FILES),
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
fn read_artifact(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Classify a file's provenance against the manifest digest recorded for it.
fn provenance(tracked: Option<&String>, on_disk: &str) -> ArtifactProvenance {
    match tracked {
        Some(digest) if *digest == sha256_hex(on_disk.as_bytes()) => {
            ArtifactProvenance::OrbitWritten
        }
        Some(_) => ArtifactProvenance::LocallyModified,
        None => ArtifactProvenance::UserAuthored,
    }
}

fn diagnose_catalog(runtime: &OrbitRuntime, catalog: &ManagedCatalog) -> ArtifactHealth {
    let kind = catalog.kind;
    let mut findings = Vec::new();

    if !catalog.dir.is_dir() {
        // Activities also live in workspace / env catalog dirs. A missing
        // managed directory must not hide those production-path faults.
        if kind != ArtifactKind::Activity {
            return ArtifactHealth {
                kind,
                scanned: 0,
                findings,
            };
        }
        return finish_catalog_health(runtime, catalog, findings, &BTreeMap::new());
    }

    let manifest_path = catalog.dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let manifest =
        match load_managed_asset_manifest(&manifest_path, kind.asset_kind(), kind.layout()) {
            Ok(manifest) => manifest,
            Err(error) => {
                // An unreadable manifest costs provenance for the whole kind, so
                // say so instead of silently reporting every artifact as
                // user-authored.
                findings.push(ArtifactFinding {
                    kind,
                    name: MANAGED_ASSET_MANIFEST_FILE.to_string(),
                    path: manifest_path,
                    condition: ArtifactCondition::Faulty,
                    provenance: ArtifactProvenance::OrbitWritten,
                    detail: format!(
                        "managed {} manifest is unreadable: {error}",
                        kind.singular()
                    ),
                    remediation: format!(
                        "Repair or move aside the {} manifest, then run `{}`.",
                        kind.singular(),
                        init_command(kind)
                    ),
                });
                return ArtifactHealth {
                    kind,
                    scanned: 0,
                    findings,
                };
            }
        };
    let tracked: BTreeMap<String, String> = manifest
        .as_ref()
        .map(|manifest| manifest.assets.clone())
        .unwrap_or_default();

    // Deprecated: tracked by the manifest, still on disk, no longer shipped.
    for (name, digest) in &tracked {
        if catalog.shipped.contains(name) {
            continue;
        }
        let path = catalog.path_of(name);
        let Some(on_disk) = read_artifact(&path) else {
            continue;
        };
        let provenance = provenance(Some(digest), &on_disk);
        let detail = if provenance.is_removable() {
            format!(
                "`{name}` is a managed default this Orbit no longer ships; its content is \
                 unmodified, so it can be retired safely"
            )
        } else {
            format!(
                "`{name}` is a managed default this Orbit no longer ships, but it was locally \
                 modified; it will be preserved outside the active catalog rather than deleted"
            )
        };
        findings.push(ArtifactFinding {
            kind,
            name: name.clone(),
            path,
            condition: ArtifactCondition::Deprecated,
            provenance,
            detail,
            remediation: "Run `orbit doctor --fix-stale-artifacts`.".to_string(),
        });
    }

    // Stale: a managed copy of an older release, or an untracked file wearing
    // a bundled default's name.
    if let Some(embedded) = &catalog.embedded {
        for (name, rendered) in embedded {
            let path = catalog.path_of(name);
            let Some(on_disk) = read_artifact(&path) else {
                continue;
            };
            let on_disk_digest = sha256_hex(on_disk.as_bytes());
            let rendered_digest = sha256_hex(rendered.as_bytes());
            if on_disk_digest == rendered_digest {
                continue;
            }
            match tracked.get(name) {
                // Orbit's own copy, unedited, but from an older release.
                Some(digest) if *digest == on_disk_digest => findings.push(ArtifactFinding {
                    kind,
                    name: name.clone(),
                    path,
                    condition: ArtifactCondition::Stale,
                    provenance: ArtifactProvenance::OrbitWritten,
                    detail: format!(
                        "`{name}` is a stale shipped default: an Orbit-written copy of an older \
                         release that has drifted from the content this binary ships"
                    ),
                    remediation: format!("Run `{}`.", init_command(kind)),
                }),
                // Edited after Orbit wrote it: intentional local authorship,
                // not staleness. Refreshing would discard the edit, so this is
                // deliberately not reported.
                Some(_) => {}
                // Untracked file occupying a bundled default's name — the
                // collision ADR-0346 already warns about at init time.
                None if manifest.is_some() => findings.push(ArtifactFinding {
                    kind,
                    name: name.clone(),
                    path: path.clone(),
                    condition: ArtifactCondition::Stale,
                    provenance: ArtifactProvenance::UserAuthored,
                    detail: format!(
                        "user-authored `{}` collides with bundled default `{name}`, so the \
                         bundled default is not installed",
                        path.display()
                    ),
                    remediation: format!(
                        "Move or rename `{}`, then run `{}` to install the bundled default.",
                        path.display(),
                        init_command(kind)
                    ),
                }),
                None => {}
            }
        }
    }

    // Missing: a previously reconciled catalog no longer has a primary shipped
    // default on disk. The stale loop above skips absent files, and loaders
    // only inspect what remains, so without this check a deleted default is
    // invisible — including after a warm open that trusted the defaults stamp.
    if manifest.is_some() {
        for name in &catalog.shipped {
            if !is_primary_shipped_asset(kind, name) {
                continue;
            }
            let path = catalog.path_of(name);
            if path.is_file() {
                continue;
            }
            findings.push(ArtifactFinding {
                kind,
                name: name.clone(),
                path,
                condition: ArtifactCondition::Missing,
                provenance: ArtifactProvenance::OrbitWritten,
                detail: format!(
                    "`{name}` is a shipped {} default this Orbit still embeds, but the managed \
                     file is absent",
                    kind.singular()
                ),
                remediation: format!("Run `{}`.", init_command(kind)),
            });
        }
    }

    finish_catalog_health(runtime, catalog, findings, &tracked)
}

/// The catalog entry dispatch actually loads: a YAML stem, or a skill's
/// `SKILL.md`. Reference files under a skill tree are not independent
/// definitions, so a missing one is not reported here.
fn is_primary_shipped_asset(kind: ArtifactKind, name: &str) -> bool {
    match kind {
        ArtifactKind::Skill => Path::new(name)
            .file_name()
            .is_some_and(|file_name| file_name == "SKILL.md"),
        ArtifactKind::Job
        | ArtifactKind::Activity
        | ArtifactKind::AutoTask
        | ArtifactKind::Routine => true,
    }
}

fn finish_catalog_health(
    runtime: &OrbitRuntime,
    catalog: &ManagedCatalog,
    mut findings: Vec<ArtifactFinding>,
    tracked: &BTreeMap<String, String>,
) -> ArtifactHealth {
    let kind = catalog.kind;
    let (scanned, faults) = collect_faults(runtime, catalog);
    for fault in faults {
        let provenance = read_artifact(&fault.path)
            .map(|on_disk| provenance(tracked.get(&fault.name), &on_disk))
            .unwrap_or(ArtifactProvenance::UserAuthored);
        let stale_shipped_default = findings.iter().any(|finding| {
            finding.name == fault.name
                && finding.condition == ArtifactCondition::Stale
                && finding.provenance == ArtifactProvenance::OrbitWritten
        });
        let remediation = if fault.condition == ArtifactCondition::Residual {
            format!(
                "Review the residual skill directory at `{}` and remove it manually if it is no longer needed.",
                fault.path.display()
            )
        } else if let Some(command) = fault.repair_command {
            format!("Run `{command}`.")
        } else if stale_shipped_default {
            format!("Run `{}`.", init_command(kind))
        } else if provenance == ArtifactProvenance::OrbitWritten {
            format!(
                "A shipped {} default failed to load — reinstall or upgrade orbit, then run `{}`.",
                kind.singular(),
                init_command(kind)
            )
        } else {
            format!(
                "Fix the {} definition at `{}` (or move it aside), then rerun `orbit doctor`.",
                kind.singular(),
                fault.path.display()
            )
        };
        findings.push(ArtifactFinding {
            kind,
            name: fault.name,
            path: fault.path,
            condition: fault.condition,
            provenance,
            detail: fault.detail,
            remediation,
        });
    }

    findings.sort_by(|left, right| {
        (left.condition.as_str(), &left.name).cmp(&(right.condition.as_str(), &right.name))
    });
    ArtifactHealth {
        kind,
        scanned,
        findings,
    }
}

struct LoadFault {
    name: String,
    path: PathBuf,
    condition: ArtifactCondition,
    detail: String,
    repair_command: Option<&'static str>,
}

impl From<ActivityCatalogFault> for LoadFault {
    fn from(fault: ActivityCatalogFault) -> Self {
        Self {
            name: fault.name,
            path: fault.path,
            condition: ArtifactCondition::Faulty,
            detail: fault.detail,
            repair_command: fault.repair_command,
        }
    }
}

/// Load every artifact of one kind through its real loader, returning
/// `(inspected count, failures)`. Using the production loader is the point:
/// doctor must report exactly what dispatch would find, not a parallel parse.
fn collect_faults(runtime: &OrbitRuntime, catalog: &ManagedCatalog) -> (usize, Vec<LoadFault>) {
    if catalog.kind == ArtifactKind::Activity {
        let (scanned, faults) = collect_activity_catalog_faults(runtime);
        return (scanned, faults.into_iter().map(LoadFault::from).collect());
    }

    let mut faults = Vec::new();

    if catalog.kind == ArtifactKind::Skill {
        let rows = match runtime.skill_catalog().doctor() {
            Ok(rows) => rows,
            Err(error) => {
                faults.push(LoadFault {
                    name: "<catalog>".to_string(),
                    path: catalog.dir.clone(),
                    condition: ArtifactCondition::Faulty,
                    detail: format!("cannot enumerate skills: {error}"),
                    repair_command: None,
                });
                return (0, faults);
            }
        };
        let scanned = rows.len();
        for row in rows {
            match row.status {
                crate::skill_catalog::SkillCatalogDoctorStatus::Ok => {}
                crate::skill_catalog::SkillCatalogDoctorStatus::Warning => {
                    faults.push(LoadFault {
                        name: row.skill_id,
                        path: row.path,
                        condition: ArtifactCondition::Residual,
                        detail: row.message,
                        repair_command: None,
                    });
                }
                crate::skill_catalog::SkillCatalogDoctorStatus::Error => {
                    faults.push(LoadFault {
                        name: format!("{}/SKILL.md", row.skill_id),
                        path: row.path.join("SKILL.md"),
                        condition: ArtifactCondition::Faulty,
                        detail: row.message,
                        repair_command: None,
                    });
                }
            }
        }
        return (scanned, faults);
    }

    if catalog.kind == ArtifactKind::AutoTask {
        // The auto-task loader owns rules beyond parsing (the file stem must
        // equal the definition name), so reuse it wholesale.
        let collection = collect_auto_tasks(&runtime.paths().local_dir);
        let scanned = collection.definitions.len() + collection.errors.len();
        for error in collection.errors {
            let path = error.path.unwrap_or_else(|| catalog.dir.clone());
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string();
            faults.push(LoadFault {
                name,
                path,
                condition: ArtifactCondition::Faulty,
                detail: error.message,
                repair_command: None,
            });
        }
        return (scanned, faults);
    }

    let Ok(entries) = std::fs::read_dir(&catalog.dir) else {
        return (0, faults);
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
                })
        })
        .collect();
    paths.sort();

    let scanned = paths.len();
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let Some(raw) = read_artifact(&path) else {
            faults.push(LoadFault {
                name,
                path,
                condition: ArtifactCondition::Faulty,
                detail: "file is unreadable".to_string(),
                repair_command: None,
            });
            continue;
        };
        let outcome = match catalog.kind {
            ArtifactKind::Job => load_job_asset(&raw).map(|_| ()).map_err(|e| e.to_string()),
            ArtifactKind::Routine => parse_routine_yaml(&raw)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            ArtifactKind::Activity | ArtifactKind::Skill | ArtifactKind::AutoTask => Ok(()),
        };
        if let Err(detail) = outcome {
            faults.push(LoadFault {
                name,
                path,
                condition: ArtifactCondition::Faulty,
                detail,
                repair_command: None,
            });
        }
    }
    (scanned, faults)
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
        if provenance(Some(digest), &on_disk).is_removable() {
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
                "locally modified deprecated artifact was preserved outside the active catalog"
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
        super::write_managed_asset_manifest(&manifest_path, &next)?;
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
