use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::skill_catalog::{LoadedSkill, SkillCatalogDoctorStatus};

use super::{ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets};

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
pub(crate) const DEFAULT_SKILL_FILES: [(&str, &str); 31] = [
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
        "orbit/references/docs-corpus.md",
        include_str!("../../assets/skills/orbit/references/docs-corpus.md"),
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
        "orbit-setup/references/automation.md",
        include_str!("../../assets/skills/orbit-setup/references/automation.md"),
    ),
    (
        "orbit-setup/references/auto-tasks.md",
        include_str!("../../assets/skills/orbit-setup/references/auto-tasks.md"),
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

pub(crate) fn is_default_skill_file_for_root(
    skill_id: &str,
    path: &Path,
    orbit_root: &Path,
) -> Result<bool, OrbitError> {
    let Some((_, content)) = default_skill_files()
        .into_iter()
        .find(|(default_id, _)| *default_id == skill_id)
    else {
        return Ok(false);
    };
    if !path.exists() {
        return Ok(false);
    }
    let existing = std::fs::read_to_string(path).map_err(|e| OrbitError::Io(e.to_string()))?;
    Ok(existing == inject_skill_template_tokens(content, orbit_root))
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
        if let Some(home) = crate::paths::home_dir() {
            results.extend(doctor_client_skill_links(
                &crate::bootstrap::init::skill_link_roots(&home),
            )?);
        }
        Ok(results)
    }
}

/// Report dangling/orphaned client skill symlinks under the agent
/// discovery directories (`~/.claude/skills`, `~/.agents/skills`).
///
/// Catalog doctor only walks seeded skill trees. Client CLIs discover
/// skills through these link dirs, so a leftover after a default-set
/// shrink is invisible unless this pass inspects them.
pub(crate) fn doctor_client_skill_links(
    skills_links_dirs: &[PathBuf],
) -> Result<Vec<SkillDoctorResult>, OrbitError> {
    let mut rows = Vec::new();
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
            let skill_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
                .to_string();
            rows.push(SkillDoctorResult {
                skill_name,
                status: SkillDoctorStatus::Error,
                message: format!("dangling skill link at {} (target missing)", path.display()),
            });
        }
    }
    Ok(rows)
}
