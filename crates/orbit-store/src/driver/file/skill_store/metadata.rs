//! meta.json parsing with optional metadata fields and semver validation.

use std::fs;
use std::path::Path;

use crate::json_schema::validate_schema_document;
use orbit_common::OrbitError;
use serde_json::{Map, Value};

use super::types::SkillMeta;

const META_NAME: &str = "name";
const META_SUMMARY: &str = "summary";
const META_TAGS: &str = "tags";
const META_VERSION: &str = "version";

#[derive(Debug)]
pub(super) struct ParsedMetaJson {
    pub(super) meta: Option<SkillMeta>,
    pub(super) meta_raw: Option<Value>,
    pub(super) output_schema: Option<Value>,
}

pub(super) fn parse_meta_json(path: &Path) -> Result<ParsedMetaJson, OrbitError> {
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
