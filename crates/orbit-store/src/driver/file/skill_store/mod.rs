use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::json_schema::validate_schema_document;
use crate::scope::{ScopeStrategy, ScopedStore, resolve};
use orbit_common::{NotFoundKind, OrbitError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::fs::path_safety::validate_path_stem;
use crate::fs::yaml::parse_yaml_with;

const PURPOSE_SECTION: &str = "Purpose";
const META_NAME: &str = "name";
const META_SUMMARY: &str = "summary";
const META_TAGS: &str = "tags";
const META_VERSION: &str = "version";

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

#[derive(Debug)]
struct ParsedMetaJson {
    meta: Option<SkillMeta>,
    meta_raw: Option<Value>,
    output_schema: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
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
            fs::create_dir_all(&self.root).map_err(|e| OrbitError::Io(e.to_string()))?;
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

fn parse_skill_markdown(raw: &str) -> Result<SkillSections, OrbitError> {
    validate_required_frontmatter(raw)?;

    let mut current_section: Option<String> = None;
    let mut section_map: BTreeMap<String, String> = BTreeMap::new();

    for line in raw.lines() {
        let trimmed = line.trim_end();
        if let Some(section_name) = parse_section_heading(trimmed.trim()) {
            let _ = section_map.entry(section_name.clone()).or_default();
            current_section = Some(section_name);
            continue;
        }

        let Some(section_name) = current_section.clone() else {
            continue;
        };

        let Some(entry) = section_map.get_mut(&section_name) else {
            return Err(OrbitError::SkillValidation(format!(
                "section parsing error for heading '{section_name}'"
            )));
        };
        entry.push_str(trimmed);
        entry.push('\n');
    }

    Ok(SkillSections {
        purpose: section_map
            .get(PURPOSE_SECTION)
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
        behavioral_constraints: section_map
            .get("Behavioral Constraints")
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
        output_requirements: section_map
            .get("Output Requirements")
            .map(|v| v.trim().to_string())
            .unwrap_or_default(),
        evaluation_focus: section_map
            .get("Evaluation Focus")
            .map(|v| v.trim().to_string()),
        prohibitions: section_map
            .get("Prohibitions")
            .map(|v| v.trim().to_string()),
        examples: section_map.get("Examples").map(|v| v.trim().to_string()),
    })
}

fn validate_required_frontmatter(raw: &str) -> Result<(), OrbitError> {
    let fm = parse_frontmatter(raw)?;

    let Some(name) = fm.name else {
        return Err(OrbitError::SkillValidation(
            "missing required frontmatter field 'name'".to_string(),
        ));
    };
    if name.trim().is_empty() {
        return Err(OrbitError::SkillValidation(
            "frontmatter field 'name' must not be empty".to_string(),
        ));
    }

    let Some(description) = fm.description else {
        return Err(OrbitError::SkillValidation(
            "missing required frontmatter field 'description'".to_string(),
        ));
    };
    if description.trim().is_empty() {
        return Err(OrbitError::SkillValidation(
            "frontmatter field 'description' must not be empty".to_string(),
        ));
    }

    Ok(())
}

fn parse_frontmatter(raw: &str) -> Result<SkillFrontmatter, OrbitError> {
    let mut lines = raw.lines();
    let Some(first_line) = lines.next() else {
        return Err(OrbitError::SkillValidation(
            "missing frontmatter block".to_string(),
        ));
    };
    if first_line.trim() != "---" {
        return Err(OrbitError::SkillValidation(
            "missing frontmatter block".to_string(),
        ));
    }

    let mut fm_lines: Vec<&str> = Vec::new();
    let mut found_end = false;
    for line in lines {
        if line.trim() == "---" {
            found_end = true;
            break;
        }
        fm_lines.push(line);
    }

    if !found_end {
        return Err(OrbitError::SkillValidation(
            "unterminated frontmatter block".to_string(),
        ));
    }

    let fm_raw = fm_lines.join("\n");
    parse_yaml_with(&fm_raw, Path::new("<skill frontmatter>"), |_, e| {
        OrbitError::SkillValidation(format!("invalid skill frontmatter: {e}"))
    })
}

fn parse_section_heading(raw: &str) -> Option<String> {
    if !raw.starts_with('#') {
        return None;
    }
    let title = raw.trim_start_matches('#').trim();
    if title.is_empty() {
        return None;
    }
    Some(title.to_string())
}

fn parse_meta_json(path: &Path) -> Result<ParsedMetaJson, OrbitError> {
    let raw = fs::read_to_string(path).map_err(|e| OrbitError::Io(e.to_string()))?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| {
        OrbitError::SkillValidation(format!("invalid meta.json at '{}': {e}", path.display()))
    })?;
    let obj = value.as_object().ok_or_else(|| {
        OrbitError::SkillValidation(format!(
            "meta.json at '{}' must be a JSON object",
            path.display()
        ))
    })?;

    let mut schema_obj = obj.clone();
    let name = parse_optional_string(&mut schema_obj, META_NAME)?;
    let summary = parse_optional_string(&mut schema_obj, META_SUMMARY)?;
    let tags = parse_optional_tags(&mut schema_obj)?;
    let version = parse_optional_semver(&mut schema_obj, META_VERSION)?;
    let meta = if name.is_some() || summary.is_some() || !tags.is_empty() || version.is_some() {
        Some(SkillMeta {
            name,
            summary,
            tags,
            version,
        })
    } else {
        None
    };

    let output_schema = Value::Object(schema_obj);
    let schema_context = format!("meta.json at '{}'", path.display());
    let _ = validate_schema_document(&output_schema, &schema_context)?;

    Ok(ParsedMetaJson {
        meta,
        meta_raw: Some(value),
        output_schema: Some(output_schema),
    })
}

fn parse_optional_string(
    obj: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(value) = obj.remove(key) else {
        return Ok(None);
    };
    let string = value.as_str().ok_or_else(|| {
        OrbitError::SkillValidation(format!("meta.json field '{}' must be a string", key))
    })?;
    Ok(Some(string.to_string()))
}

fn parse_optional_tags(obj: &mut Map<String, Value>) -> Result<Vec<String>, OrbitError> {
    let Some(value) = obj.remove(META_TAGS) else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        OrbitError::SkillValidation("meta.json field 'tags' must be an array".to_string())
    })?;
    let mut tags = Vec::new();
    for tag in values {
        let item = tag.as_str().ok_or_else(|| {
            OrbitError::SkillValidation("meta.json field 'tags' must contain strings".to_string())
        })?;
        tags.push(item.to_string());
    }
    Ok(tags)
}

fn parse_optional_semver(
    obj: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(value) = obj.remove(key) else {
        return Ok(None);
    };
    let version = value.as_str().ok_or_else(|| {
        OrbitError::SkillValidation(format!("meta.json field '{}' must be a string", key))
    })?;
    if !is_semver(version) {
        return Err(OrbitError::SkillValidation(format!(
            "meta.json field '{}' must be semantic version MAJOR.MINOR.PATCH",
            key
        )));
    }
    Ok(Some(version.to_string()))
}

fn is_semver(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
#[cfg(test)]
mod tests;
