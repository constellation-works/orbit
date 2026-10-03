//! Skill catalog DTOs.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SkillSections {
    pub purpose: String,
    pub behavioral_constraints: String,
    pub output_requirements: String,
    pub evaluation_focus: Option<String>,
    pub prohibitions: Option<String>,
    pub examples: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SkillMeta {
    pub name: Option<String>,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LoadedSkill {
    pub id: String,
    pub path: PathBuf,
    pub content_hash: String,
    pub content: String,
    pub sections: SkillSections,
    pub meta: Option<SkillMeta>,
    pub meta_raw: Option<Value>,
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum SkillCatalogDoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SkillCatalogDoctorRow {
    pub skill_id: String,
    pub path: PathBuf,
    pub status: SkillCatalogDoctorStatus,
    pub message: String,
}
