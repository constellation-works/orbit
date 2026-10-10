//! Diagnosis half of definition-artifact health: classify one managed
//! catalog's files and collect load faults through the production loaders.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_engine::activity_job::load_job_asset;

use super::activity_catalog::{
    ActivityCatalogFault, collect_activity_catalog_faults, read_confined_text,
};
use super::artifact::{
    ArtifactCondition, ArtifactFinding, ArtifactHealth, ArtifactKind, ArtifactProvenance,
    ManagedCatalog, init_command, provenance, read_artifact,
};
use crate::OrbitRuntime;
use crate::application::auto_tasks::collect_auto_tasks;
use crate::application::auto_tasks::settings::classify_bundled_file;
use crate::application::managed_assets::{
    ConfinedAssetPath, MANAGED_ASSET_MANIFEST_FILE, load_managed_asset_manifest,
    resolve_confined_asset_path,
};
use crate::application::routines::seed::RETIRED_ROUTINE_FILES;
use crate::application::routines::template::{ShippedShape, shipped_shape_of};
use orbit_automation::auto_tasks::settings::{AutoTaskOverrides, load_settings_table};

pub(super) fn diagnose_catalog(runtime: &OrbitRuntime, catalog: &ManagedCatalog) -> ArtifactHealth {
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
    // Shipped defaults an operator deleted are an explicit choice, not drift.
    let opted_out: BTreeSet<String> = manifest
        .as_ref()
        .map(|manifest| manifest.opted_out.clone())
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
        let provenance = provenance(kind, name, Some(digest), &on_disk);
        let (detail, remediation) = match resolve_confined_asset_path(
            &catalog.dir,
            &kind.layout().relative_path(name),
        ) {
            Ok(ConfinedAssetPath::Unsafe(component)) => (
                format!(
                    "`{name}` is a retired managed default, but retirement is skipped because \
                     '{}' is linked or is not the expected file or directory type",
                    component.display()
                ),
                "Review the linked or non-file path, restore a confined regular asset, then run \
                 `orbit doctor --fix-stale-artifacts`."
                    .to_string(),
            ),
            Err(error) => (
                format!(
                    "`{name}` is a retired managed default, but retirement is refused: {error}"
                ),
                "Repair the managed catalog path, then run `orbit doctor --fix-stale-artifacts`."
                    .to_string(),
            ),
            _ if provenance.is_removable() => (
                format!(
                    "`{name}` is a managed default this Orbit no longer ships; its content is \
                     unmodified, so it can be retired safely"
                ),
                "Run `orbit doctor --fix-stale-artifacts`.".to_string(),
            ),
            _ => (
                format!(
                    "`{name}` is a managed default this Orbit no longer ships, but it was locally \
                     modified; it will be preserved outside the active catalog rather than deleted"
                ),
                "Run `orbit doctor --fix-stale-artifacts`.".to_string(),
            ),
        };
        findings.push(ArtifactFinding {
            kind,
            name: name.clone(),
            path,
            condition: ArtifactCondition::Deprecated,
            provenance,
            detail,
            remediation,
        });
    }

    // Deprecated without a manifest entry: a retired default a prior release
    // wrote but never recorded. The loop above cannot see it and it wears no
    // shipped name, so nothing else reaches it — and the catalog would read
    // healthy while a dead definition sits in it, contradicting the retired
    // row `orbit routine list` shows on every pass [DANI-10502]. `orbit
    // workspace sync` judges the same file by content and retires it.
    if kind == ArtifactKind::Routine {
        for (name, _) in RETIRED_ROUTINE_FILES {
            if tracked.contains_key(*name) || catalog.shipped.contains(*name) {
                continue;
            }
            let path = catalog.path_of(name);
            let Some(on_disk) = read_artifact(&path) else {
                continue;
            };
            if shipped_shape_of(name, &on_disk) != Some(ShippedShape::Retired) {
                continue;
            }
            findings.push(ArtifactFinding {
                kind,
                name: (*name).to_string(),
                path,
                condition: ArtifactCondition::Deprecated,
                // No recorded digest, so nothing proves Orbit wrote these
                // exact bytes: retirement keeps a copy rather than deleting,
                // which is why the repair flag is not the remediation here.
                provenance: ArtifactProvenance::UserAuthored,
                detail: format!(
                    "`{name}` matches a managed default this Orbit no longer ships but is absent \
                     from the managed manifest; it stays in the active catalog until it is \
                     retired"
                ),
                remediation: format!(
                    "Run `{}` to retire it and keep a copy outside the active catalog.",
                    init_command(kind)
                ),
            });
        }
    }

    // Stale: a managed copy of an older release, or an untracked file wearing
    // a bundled default's name.
    if let Some(embedded) = &catalog.embedded {
        // An unreadable settings table is reported by the loader's faults.
        let auto_task_settings = if kind == ArtifactKind::AutoTask {
            load_settings_table(&catalog.dir).unwrap_or_default()
        } else {
            Default::default()
        };
        for (name, rendered) in embedded {
            if opted_out.contains(name) {
                continue;
            }
            let path = catalog.path_of(name);
            let Some(on_disk) = read_artifact(&path) else {
                continue;
            };
            let on_disk_digest = sha256_hex(on_disk.as_bytes());
            let rendered_digest = sha256_hex(rendered.as_bytes());
            if on_disk_digest == rendered_digest {
                continue;
            }
            if kind == ArtifactKind::AutoTask
                && manifest.is_some()
                && let Ok(Some(overrides)) = classify_bundled_file(
                    rendered,
                    &on_disk,
                    tracked.get(name),
                    auto_task_settings.get(name),
                    runtime.workspace_base_branch(),
                )
            {
                findings.push(auto_task_fork_finding(
                    name,
                    path,
                    tracked.contains_key(name),
                    &overrides.overrides,
                    overrides.stale_shipped_body,
                ));
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
            if !is_primary_shipped_asset(kind, name) || opted_out.contains(name) {
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

/// Report a shipped auto-task default whose file left its bundled body.
///
/// A settings-only fork is stale: sync moves its settings into the settings
/// table. A body fork is preserved as authored; the finding names the body
/// fields so the operator can decide whether to keep them. Neither remedy
/// moves or renames the file, which would discard its settings.
fn auto_task_fork_finding(
    name: &str,
    path: PathBuf,
    tracked: bool,
    overrides: &AutoTaskOverrides,
    stale_shipped_body: bool,
) -> ArtifactFinding {
    let kind = ArtifactKind::AutoTask;
    let provenance = if tracked {
        ArtifactProvenance::LocallyModified
    } else {
        ArtifactProvenance::UserAuthored
    };
    let settings = overrides.settings.field_names();
    let settings_clause = if settings.is_empty() {
        String::new()
    } else {
        format!("; operator settings: {}", settings.join(", "))
    };
    if overrides.body_fields.is_empty() {
        return ArtifactFinding {
            kind,
            name: name.to_string(),
            path,
            condition: ArtifactCondition::Stale,
            provenance,
            detail: if stale_shipped_body {
                format!("`{name}` has a stale shipped body (will upgrade on sync){settings_clause}")
            } else {
                format!(
                    "`{name}` differs from its bundled default only in settings{settings_clause}, \
                     so Orbit stopped refreshing its body"
                )
            },
            remediation: format!(
                "Run `{}` to move the settings into the auto-task settings table and manage the bundled body again.",
                init_command(kind)
            ),
        };
    }
    ArtifactFinding {
        kind,
        name: name.to_string(),
        path,
        condition: ArtifactCondition::Forked,
        provenance,
        detail: format!(
            "`{name}` is a body fork of its bundled default; body fields: {}{settings_clause}. \
             Orbit preserves it and no longer applies upstream template changes to it",
            overrides.body_fields.join(", ")
        ),
        remediation: format!(
            "Keep the fork to retain its body edits, or set those body fields back to the bundled values with `orbit auto-task update {name}` and run `{}` to manage the body again; its settings are kept.",
            init_command(kind)
        ),
    }
}

/// Whether `path` is the managed catalog file for `name`.
///
/// A directory walk that canonicalizes its root (auto-tasks) still matches.
/// Only parent directories are canonicalized, so a symlink file in another
/// catalog cannot borrow this location by pointing at the managed bytes.
fn is_managed_artifact_location(catalog: &ManagedCatalog, name: &str, path: &Path) -> bool {
    same_catalog_file(&catalog.path_of(name), path)
}

fn same_catalog_file(managed: &Path, path: &Path) -> bool {
    if managed == path {
        return true;
    }
    let (Some(managed_name), Some(path_name)) = (managed.file_name(), path.file_name()) else {
        return false;
    };
    if managed_name != path_name {
        return false;
    }
    let (Some(managed_dir), Some(path_dir)) = (managed.parent(), path.parent()) else {
        return false;
    };
    managed_dir == path_dir
        || matches!(
            (
                std::fs::canonicalize(managed_dir),
                std::fs::canonicalize(path_dir)
            ),
            (Ok(managed_dir), Ok(path_dir)) if managed_dir == path_dir
        )
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
        // Activity faults can name links or special files the confined
        // catalog scan refused to read; provenance must not read them either.
        let on_disk = if kind == ArtifactKind::Activity {
            read_confined_text(&fault.path)
        } else {
            read_artifact(&fault.path)
        };
        // Digests and stale-default remediation describe the managed file at
        // its managed path. A same-named activity in a workspace or explicit
        // catalog is a different file and keeps its own provenance.
        let recorded = is_managed_artifact_location(catalog, &fault.name, &fault.path)
            .then(|| tracked.get(&fault.name))
            .flatten();
        let provenance = on_disk
            .map(|on_disk| provenance(kind, &fault.name, recorded, &on_disk))
            .unwrap_or(ArtifactProvenance::UserAuthored);
        let stale_shipped_default = findings.iter().any(|finding| {
            finding.name == fault.name
                && finding.condition == ArtifactCondition::Stale
                && finding.provenance == ArtifactProvenance::OrbitWritten
                && same_catalog_file(&finding.path, &fault.path)
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
