use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::skill_catalog::{LoadedSkill, SkillCatalogDoctorStatus};

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

pub(super) fn list_file_skills(rt: &OrbitRuntime) -> Result<Vec<LoadedSkill>, OrbitError> {
    rt.skill_catalog().read().skill().list()
}

pub(super) fn show_file_skill(rt: &OrbitRuntime, name: &str) -> Result<LoadedSkill, OrbitError> {
    rt.skill_catalog().read().skill().get(name)
}

pub(super) fn doctor_file_skills(rt: &OrbitRuntime) -> Result<Vec<SkillDoctorResult>, OrbitError> {
    let rows = rt.skill_catalog().read().skill().doctor()?;
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
    let global_root = rt.global_root();
    if let Some(discovery_base) = global_root.parent() {
        results.extend(doctor_client_skill_links(&skill_link_roots(
            discovery_base,
        ))?);
    }
    Ok(results)
}

/// The client skill-discovery directories under `base_root`.
pub(crate) fn skill_link_roots(base_root: &Path) -> Vec<PathBuf> {
    [".agents", ".claude"]
        .into_iter()
        .map(|dir| base_root.join(dir).join("skills"))
        .collect()
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
