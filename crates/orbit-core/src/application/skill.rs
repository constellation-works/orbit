use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::skill_catalog::{LoadedSkill, SkillCatalogDoctorStatus};

use super::managed_assets::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};

/// Every shipped skill file, keyed by its path relative to the skills root.
///
/// Skills are directory trees rather than single documents, so their managed
/// manifest keys on the relative path ([`ManagedAssetLayout::RelativePath`])
/// instead of a bare definition name.
///
/// Routers separate everyday task work, backlog orchestration and machine setup.
/// References load on demand and may link across the bundled skill trees. The
/// ordering below groups each skill's files under its router, in the order its
/// reference table presents them.
pub(crate) const DEFAULT_SKILL_FILES: [(&str, &str); 32] = [
    // Everyday task work: the router, then its references in table order.
    (
        "orbit/SKILL.md",
        include_str!("../../assets/skills/orbit/SKILL.md"),
    ),
    (
        "orbit/references/task-execution.md",
        include_str!("../../assets/skills/orbit/references/task-execution.md"),
    ),
    (
        "orbit/references/task-fields.md",
        include_str!("../../assets/skills/orbit/references/task-fields.md"),
    ),
    (
        "orbit/references/task-authoring.md",
        include_str!("../../assets/skills/orbit/references/task-authoring.md"),
    ),
    (
        "orbit/references/task-review.md",
        include_str!("../../assets/skills/orbit/references/task-review.md"),
    ),
    (
        "orbit/references/search.md",
        include_str!("../../assets/skills/orbit/references/search.md"),
    ),
    (
        "orbit/references/friction.md",
        include_str!("../../assets/skills/orbit/references/friction.md"),
    ),
    (
        "orbit/references/tool-surface.md",
        include_str!("../../assets/skills/orbit/references/tool-surface.md"),
    ),
    (
        "orbit/references/concepts.md",
        include_str!("../../assets/skills/orbit/references/concepts.md"),
    ),
    (
        "orbit/references/setup/distributed-drain.md",
        include_str!("../../assets/skills/orbit/references/setup/distributed-drain.md"),
    ),
    // The orchestrator's operating loop, layered on the primitives above.
    (
        "orbit-orchestrate/SKILL.md",
        include_str!("../../assets/skills/orbit-orchestrate/SKILL.md"),
    ),
    (
        "orbit-orchestrate/references/loop.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop.md"),
    ),
    (
        "orbit-orchestrate/references/authorization.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/authorization.md"),
    ),
    (
        "orbit-orchestrate/references/orchestration.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/orchestration.md"),
    ),
    (
        "orbit-orchestrate/references/workflows.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/workflows.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/run-debugging.md"),
    ),
    (
        "orbit-orchestrate/references/common-failures.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/common-failures.md"),
    ),
    (
        "orbit-orchestrate/references/recovery.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery.md"),
    ),
    (
        "orbit-orchestrate/references/walkthroughs.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/walkthroughs.md"),
    ),
    // Setting Orbit up on a machine or repository.
    (
        "orbit-setup/SKILL.md",
        include_str!("../../assets/skills/orbit-setup/SKILL.md"),
    ),
    (
        "orbit-setup/references/first-run.md",
        include_str!("../../assets/skills/orbit-setup/references/first-run.md"),
    ),
    (
        "orbit-setup/references/configuration.md",
        include_str!("../../assets/skills/orbit-setup/references/configuration.md"),
    ),
    (
        "orbit-setup/references/linux-sandbox.md",
        include_str!("../../assets/skills/orbit-setup/references/linux-sandbox.md"),
    ),
    (
        "orbit-setup/references/windows-wsl2.md",
        include_str!("../../assets/skills/orbit-setup/references/windows-wsl2.md"),
    ),
    (
        "orbit-setup/references/automation.md",
        include_str!("../../assets/skills/orbit-setup/references/automation.md"),
    ),
    (
        "orbit-setup/references/auto-tasks.md",
        include_str!("../../assets/skills/orbit-setup/references/auto-tasks.md"),
    ),
    (
        "orbit-setup/references/plugins.md",
        include_str!("../../assets/skills/orbit-setup/references/plugins.md"),
    ),
    (
        "orbit-setup/references/remote-access.md",
        include_str!("../../assets/skills/orbit-setup/references/remote-access.md"),
    ),
    (
        "orbit-setup/references/multi-host.md",
        include_str!("../../assets/skills/orbit-setup/references/multi-host.md"),
    ),
    (
        "orbit-setup/references/publication.md",
        include_str!("../../assets/skills/orbit-setup/references/publication.md"),
    ),
    (
        "orbit-setup/references/maintenance.md",
        include_str!("../../assets/skills/orbit-setup/references/maintenance.md"),
    ),
    (
        "orbit-setup/references/operational-logs.md",
        include_str!("../../assets/skills/orbit-setup/references/operational-logs.md"),
    ),
];

/// The `SKILL.md` entry point of every shipped skill, as `(id, content)`.
/// A skill id is the first path component of its managed asset paths.
pub(crate) fn default_skill_files() -> Vec<(&'static str, &'static str)> {
    DEFAULT_SKILL_FILES
        .iter()
        .filter_map(|(relative, content)| {
            relative.strip_suffix("/SKILL.md").map(|id| (id, *content))
        })
        .collect()
}

use crate::paths::ORBIT_ROOT_TOKEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct SkillDoctorResult {
    pub skill_name: String,
    pub status: SkillDoctorStatus,
    pub message: String,
}

pub(crate) fn default_skill_ids() -> Vec<&'static str> {
    default_skill_files()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Materialize the shipped skill trees under `skills_root`, recording the
/// digest Orbit wrote for each file so a skill (or a single reference file)
/// dropped from a later release can be retired by content provenance.
///
/// The digest covers the *rendered* document: `ORBIT_ROOT_TOKEN` resolves to
/// the absolute root before the write, so an unchanged release re-seeds as a
/// no-op on the same root.
// ADR-0366: skills are managed by relative path, so a single reference
// file can be retired independently of its SKILL.md.
pub(crate) fn seed_default_skills(
    skills_root: &Path,
    orbit_root: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        skills_root,
        "skill",
        ManagedAssetLayout::RelativePath,
        &DEFAULT_SKILL_FILES,
        overwrite,
        |_, content| {
            Ok(Cow::Owned(inject_skill_template_tokens(
                content, orbit_root,
            )))
        },
    )
}

/// A legacy workspace skill may be removed only when every entry in its tree
/// is a shipped file with unchanged bytes. Missing shipped references are
/// allowed because older releases seeded smaller trees; unknown entries and
/// symlinks are treated as operator-owned content.
pub(crate) fn is_default_skill_tree_for_root(
    skill_id: &str,
    skill_dir: &Path,
    orbit_root: &Path,
) -> Result<bool, OrbitError> {
    let prefix = format!("{skill_id}/");
    let expected: Vec<_> = DEFAULT_SKILL_FILES
        .iter()
        .filter_map(|(path, content)| path.strip_prefix(&prefix).map(|path| (path, *content)))
        .collect();
    if expected.is_empty() {
        return Ok(false);
    }
    let metadata = match std::fs::symlink_metadata(skill_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(OrbitError::Io(error.to_string())),
    };
    if !metadata.file_type().is_dir() {
        return Ok(false);
    }

    let mut found_router = false;
    let mut pending = vec![skill_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| OrbitError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(skill_dir)
                .map_err(|e| OrbitError::Io(e.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|e| OrbitError::Io(e.to_string()))?;
            if file_type.is_dir() {
                if !expected
                    .iter()
                    .any(|(name, _)| Path::new(name).starts_with(relative))
                {
                    return Ok(false);
                }
                pending.push(path);
            } else if file_type.is_file() {
                let Some((_, content)) = expected
                    .iter()
                    .find(|(name, _)| Path::new(name) == relative)
                else {
                    return Ok(false);
                };
                let existing = std::fs::read(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
                if existing != inject_skill_template_tokens(content, orbit_root).as_bytes() {
                    return Ok(false);
                }
                found_router |= relative == Path::new("SKILL.md");
            } else {
                return Ok(false);
            }
        }
    }
    Ok(found_router)
}

pub(crate) fn inject_skill_template_tokens(raw: &str, orbit_root: &Path) -> String {
    let orbit_root_value = orbit_root.to_string_lossy();
    raw.replace(ORBIT_ROOT_TOKEN, orbit_root_value.as_ref())
}

impl OrbitRuntime {
    pub fn list_file_skills(&self) -> Result<Vec<LoadedSkill>, OrbitError> {
        self.skill_catalog().list()
    }

    pub fn show_file_skill(&self, name: &str) -> Result<LoadedSkill, OrbitError> {
        self.skill_catalog().load(name)
    }

    pub fn doctor_file_skills(&self) -> Result<Vec<SkillDoctorResult>, OrbitError> {
        let rows = self.skill_catalog().doctor()?;
        let mut results: Vec<SkillDoctorResult> = rows
            .into_iter()
            .map(|row| SkillDoctorResult {
                skill_name: row.skill_id,
                status: match row.status {
                    SkillCatalogDoctorStatus::Ok => SkillDoctorStatus::Ok,
                    SkillCatalogDoctorStatus::Warning => SkillDoctorStatus::Warning,
                    SkillCatalogDoctorStatus::Error => SkillDoctorStatus::Error,
                },
                message: row.message,
            })
            .collect();
        let global_root = self.global_root();
        if let Some(discovery_base) = global_root.parent() {
            results.extend(doctor_client_skill_links(
                &crate::bootstrap::init::skill_link_roots(discovery_base),
            )?);
        }
        Ok(results)
    }
}

/// Report dangling/orphaned client skill symlinks under the agent
/// discovery directories beside the selected global root.
///
/// Catalog doctor only walks seeded skill trees. Client CLIs discover
/// skills through these link dirs, so a leftover after a default-set
/// shrink is invisible unless this pass inspects them.
pub(crate) fn doctor_client_skill_links(
    skills_links_dirs: &[PathBuf],
) -> Result<Vec<SkillDoctorResult>, OrbitError> {
    Ok(dangling_client_skill_links(skills_links_dirs)?
        .into_iter()
        .map(|path| SkillDoctorResult {
            skill_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
                .to_string(),
            status: SkillDoctorStatus::Warning,
            message: format!(
                "dangling skill link at {} (target missing). {}",
                path.display(),
                skill_link_remediation(&path),
            ),
        })
        .collect())
}

pub(crate) fn skill_link_remediation(path: &Path) -> String {
    format!(
        "Restore the skill target or manually remove the dangling symlink at `{}`.",
        path.display(),
    )
}

/// Inspect only immediate symlinks, without modifying discovery entries or targets.
pub(crate) fn dangling_client_skill_links(
    skills_links_dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, OrbitError> {
    let mut links = Vec::new();
    for dir in skills_links_dirs {
        if !dir.exists() {
            continue;
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))? {
            paths.push(entry.map_err(|e| OrbitError::Io(e.to_string()))?.path());
        }
        paths.sort();
        for path in paths {
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
            if !meta.file_type().is_symlink() {
                continue;
            }
            if path.exists() {
                continue;
            }
            links.push(path);
        }
    }
    Ok(links)
}
