//! SkillCatalog listing, loading, doctor rows and layered scoped-store resolution.

use std::fs;
use std::path::{Path, PathBuf};

use crate::scope::{ScopeStrategy, ScopedStore, resolve};
use orbit_common::security::release::sha256_hex;
use orbit_common::{NotFoundKind, OrbitError};

use super::markdown::parse_skill_markdown;
use super::metadata::{ParsedMetaJson, parse_meta_json};
use super::types::{LoadedSkill, SkillCatalogDoctorRow, SkillCatalogDoctorStatus};
use crate::fs::path_safety::validate_path_stem;

#[derive(Debug, Clone)]
pub struct SkillCatalog {
    root: PathBuf,
    global_root: Option<PathBuf>,
}

impl SkillCatalog {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            global_root: None,
        }
    }

    /// Create a layered skill catalog. Skills use MergeByKey semantics:
    /// workspace entries override same-named global defaults.
    pub fn layered(workspace_root: PathBuf, global_root: PathBuf) -> Self {
        Self {
            root: workspace_root,
            global_root: Some(global_root),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure_layout(&self) -> Result<(), OrbitError> {
        if self.global_root.is_none() {
            orbit_common::fs::io::create_private_dir_all(&self.root)?;
        }

        // Layered catalogs read shipped global skills that bootstrap owns. Do
        // not create a caller-selected global path while opening the catalog.
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<LoadedSkill>, OrbitError> {
        let mut ids = self.list_candidate_ids()?;
        ids.sort();
        let mut skills = Vec::new();
        for id in ids {
            if let Ok(skill) = self.load(&id) {
                skills.push(skill);
            }
        }
        Ok(skills)
    }

    pub fn doctor(&self) -> Result<Vec<SkillCatalogDoctorRow>, OrbitError> {
        let mut ids = self.list_candidate_ids()?;
        ids.sort();
        let mut rows = Vec::new();
        for id in ids {
            let path = self.candidate_path(&id)?;
            match self.load(&id) {
                Ok(_) => rows.push(SkillCatalogDoctorRow {
                    skill_id: id,
                    path,
                    status: SkillCatalogDoctorStatus::Ok,
                    message: String::new(),
                }),
                Err(err) => {
                    let status = if path.is_dir() && !path.join("SKILL.md").exists() {
                        SkillCatalogDoctorStatus::Warning
                    } else {
                        SkillCatalogDoctorStatus::Error
                    };
                    rows.push(SkillCatalogDoctorRow {
                        skill_id: id,
                        path,
                        status,
                        message: err.to_string(),
                    });
                }
            }
        }
        Ok(rows)
    }

    pub fn load(&self, skill_id: &str) -> Result<LoadedSkill, OrbitError> {
        validate_skill_id(skill_id)?;

        // Skills use MergeByKey semantics: workspace wins for the named key,
        // otherwise fall through to the global default.
        match resolve::<LoadedSkill, _>(self, skill_id)? {
            Some(skill) => Ok(skill),
            None => Err(OrbitError::not_found(
                NotFoundKind::Skill,
                skill_id.to_string(),
            )),
        }
    }

    fn list_candidate_ids(&self) -> Result<Vec<String>, OrbitError> {
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

    fn candidate_path(&self, skill_id: &str) -> Result<PathBuf, OrbitError> {
        validate_skill_id(skill_id)?;
        let workspace = self.root.join(skill_id);
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

impl ScopedStore<LoadedSkill> for SkillCatalog {
    type Err = OrbitError;

    fn strategy(&self) -> ScopeStrategy {
        ScopeStrategy::MergeByKey
    }

    fn get_workspace(&self, key: &str) -> Result<Option<LoadedSkill>, OrbitError> {
        validate_skill_id(key)?;
        let dir = self.root.join(key);
        if dir.exists() {
            load_skill_from_dir(key, &dir).map(Some)
        } else {
            Ok(None)
        }
    }

    fn get_global(&self, key: &str) -> Result<Option<LoadedSkill>, OrbitError> {
        validate_skill_id(key)?;
        let Some(ref global) = self.global_root else {
            return Ok(None);
        };
        let dir = global.join(key);
        if dir.exists() {
            load_skill_from_dir(key, &dir).map(Some)
        } else {
            Ok(None)
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

/// Load a skill from a specific directory on disk.
fn load_skill_from_dir(skill_id: &str, dir: &Path) -> Result<LoadedSkill, OrbitError> {
    if !dir.is_dir() {
        return Err(OrbitError::SkillValidation(format!(
            "skill path is not a directory: {}",
            dir.display()
        )));
    }

    let skill_md_path = dir.join("SKILL.md");
    if !skill_md_path.exists() {
        return Err(OrbitError::SkillValidation(format!(
            "skill directory '{}' is missing SKILL.md for skill '{}'",
            dir.display(),
            skill_id,
        )));
    }
    let content = fs::read_to_string(&skill_md_path).map_err(|e| OrbitError::Io(e.to_string()))?;
    let sections = parse_skill_markdown(&content)?;

    let content_hash = sha256_hex(content.as_bytes());
    let meta_path = dir.join("meta.json");
    let ParsedMetaJson {
        meta,
        meta_raw,
        output_schema,
    } = if meta_path.exists() {
        parse_meta_json(&meta_path)?
    } else {
        ParsedMetaJson {
            meta: None,
            meta_raw: None,
            output_schema: None,
        }
    };

    Ok(LoadedSkill {
        id: skill_id.to_string(),
        path: dir.to_path_buf(),
        content_hash,
        content,
        sections,
        meta,
        meta_raw,
        output_schema,
    })
}

/// Collect skill candidate IDs from a single directory.
fn collect_candidate_ids(root: &Path) -> Result<Vec<String>, OrbitError> {
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
