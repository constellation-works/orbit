//! Concrete repository for skills (`SKILL.md` and optional `meta.json` per
//! skill directory).
//!
//! Reached only through [`FileSnapshot::skill`] and [`FileTx::skill`]. The
//! directory layout and the workspace-over-global lookup are
//! `driver/file/skill_store`'s; this repository owns the skill format and its
//! mapping to [`LoadedSkill`]. Skills are read-only to Orbit: bootstrap
//! installs them, so this repository has no writing methods.

mod markdown;
mod metadata;

use std::fs;
use std::marker::PhantomData;
use std::path::Path;

use orbit_common::security::release::sha256_hex;
use orbit_common::{NotFoundKind, OrbitError};

use self::markdown::parse_skill_markdown;
use self::metadata::{ParsedMetaJson, parse_meta_json};
use crate::contracts::{LoadedSkill, SkillCatalogDoctorRow, SkillCatalogDoctorStatus};
use crate::driver::file::skill_store::SkillDirs;
use crate::file_store::{FileSnapshot, FileTx};

/// Concrete skill repository scoped to a [`FileSnapshot`] or a [`FileTx`].
pub struct SkillRepository<'a, Access> {
    dirs: &'a SkillDirs,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> SkillRepository<'a, FileSnapshot<'s, SkillDirs>> {
    pub(crate) fn for_snapshot(snapshot: &'a FileSnapshot<'s, SkillDirs>) -> Self {
        Self {
            dirs: snapshot.layout(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> SkillRepository<'a, FileTx<'tx, SkillDirs>> {
    pub(crate) fn for_tx(tx: &'a FileTx<'tx, SkillDirs>) -> Self {
        Self {
            dirs: tx.layout(),
            _access: PhantomData,
        }
    }
}

impl<Access> SkillRepository<'_, Access> {
    /// Every skill that loads, ordered by id. A skill that fails to load is
    /// left out; [`Self::doctor`] reports it.
    pub fn list(&self) -> Result<Vec<LoadedSkill>, OrbitError> {
        let mut ids = self.dirs.candidate_ids()?;
        ids.sort();
        let mut skills = Vec::new();
        for id in ids {
            if let Ok(skill) = self.get(&id) {
                skills.push(skill);
            }
        }
        Ok(skills)
    }

    /// One row per candidate skill, ordered by id, saying whether it loads.
    pub fn doctor(&self) -> Result<Vec<SkillCatalogDoctorRow>, OrbitError> {
        let mut ids = self.dirs.candidate_ids()?;
        ids.sort();
        let mut rows = Vec::new();
        for id in ids {
            let path = self.dirs.candidate_path(&id)?;
            match self.get(&id) {
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

    /// The skill `skill_id`: the workspace one when present, otherwise the
    /// global default. Not found when neither exists.
    pub fn get(&self, skill_id: &str) -> Result<LoadedSkill, OrbitError> {
        let workspace = self.dirs.workspace_dir(skill_id)?;
        if workspace.exists() {
            return load_skill_from_dir(skill_id, &workspace);
        }
        match self.dirs.global_dir(skill_id)? {
            Some(global) if global.exists() => load_skill_from_dir(skill_id, &global),
            _ => Err(OrbitError::not_found(
                NotFoundKind::Skill,
                skill_id.to_string(),
            )),
        }
    }
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
