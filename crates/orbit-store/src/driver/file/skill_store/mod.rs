//! Directory layout of skills: one `<skill-id>/` directory holding `SKILL.md`
//! (and optionally `meta.json`) per skill under a workspace root, optionally
//! layered over a global root of shipped skills.

use std::fs;
use std::path::PathBuf;

use orbit_common::OrbitError;

use crate::fs::path_safety::validate_path_stem;

pub use crate::contract::{
    LoadedSkill, SkillCatalogDoctorRow, SkillCatalogDoctorStatus, SkillMeta, SkillSections,
};

/// The skill directories: a workspace root and, when layered, a global root
/// whose same-named skills the workspace overrides (MergeByKey).
#[derive(Debug, Clone)]
pub struct SkillDirs {
    root: PathBuf,
    global_root: Option<PathBuf>,
}

impl SkillDirs {
    /// Skills under `root` alone.
    pub fn single(root: PathBuf) -> Self {
        Self {
            root,
            global_root: None,
        }
    }

    /// Skills under `workspace_root`, layered over `global_root`.
    pub fn layered(workspace_root: PathBuf, global_root: PathBuf) -> Self {
        Self {
            root: workspace_root,
            global_root: Some(global_root),
        }
    }

    /// Create a single (unlayered) root. A layered root reads shipped global
    /// skills that bootstrap owns, so opening it creates no caller-selected
    /// global path.
    pub fn ensure_layout(&self) -> Result<(), OrbitError> {
        if self.global_root.is_none() {
            orbit_common::fs::io::create_private_dir_all(&self.root)?;
        }
        Ok(())
    }

    /// The workspace directory of `skill_id`.
    pub(crate) fn workspace_dir(&self, skill_id: &str) -> Result<PathBuf, OrbitError> {
        validate_skill_id(skill_id)?;
        Ok(self.root.join(skill_id))
    }

    /// The global directory of `skill_id`, when layered.
    pub(crate) fn global_dir(&self, skill_id: &str) -> Result<Option<PathBuf>, OrbitError> {
        validate_skill_id(skill_id)?;
        Ok(self
            .global_root
            .as_ref()
            .map(|global| global.join(skill_id)))
    }

    /// Every candidate skill id: the workspace's, then the global ids the
    /// workspace does not hold. Creates an unlayered root first.
    pub(crate) fn candidate_ids(&self) -> Result<Vec<String>, OrbitError> {
        self.ensure_layout()?;

        let mut ids = collect_candidate_ids(&self.root)?;

        // Merge global candidates, workspace IDs take precedence.
        if let Some(ref global) = self.global_root {
            let global_ids = collect_candidate_ids(global)?;
            let workspace_set: std::collections::HashSet<String> = ids.iter().cloned().collect();
            for id in global_ids {
                if !workspace_set.contains(&id) {
                    ids.push(id);
                }
            }
        }

        Ok(ids)
    }

    /// The directory that answers for `skill_id`: the workspace one when it
    /// exists, otherwise the global one (or the workspace one when unlayered).
    pub(crate) fn candidate_path(&self, skill_id: &str) -> Result<PathBuf, OrbitError> {
        let workspace = self.workspace_dir(skill_id)?;
        if workspace.exists() {
            Ok(workspace)
        } else {
            Ok(self
                .global_root
                .as_ref()
                .map_or(workspace, |global| global.join(skill_id)))
        }
    }
}

fn validate_skill_id(skill_id: &str) -> Result<(), OrbitError> {
    if skill_id.trim().is_empty() {
        return Err(OrbitError::SkillValidation(
            "skill id must not be empty".to_string(),
        ));
    }
    validate_path_stem(skill_id, "skill")
}

/// Collect skill candidate IDs from a single directory.
fn collect_candidate_ids(root: &std::path::Path) -> Result<Vec<String>, OrbitError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(root).map_err(|e| OrbitError::Io(e.to_string()))?;
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        ids.push(name.to_string());
    }
    Ok(ids)
}
