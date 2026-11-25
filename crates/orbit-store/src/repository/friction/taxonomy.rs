//! Workspace YAML tag taxonomy.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::governance::friction::DEFAULT_FRICTION_TAGS;

use crate::fs::yaml::{parse_yaml_with, serialize_yaml_with};

const TAGS_FILENAME: &str = "tags.yaml";

/// Seed or merge the default friction tag taxonomy into `tags.yaml`.
///
/// A missing file is created from [`DEFAULT_FRICTION_TAGS`]. An existing file
/// receives any later default tags it lacks; operator-added tags and their
/// values are left in place. Parse failures and empty taxonomies fail closed
/// without rewriting the file.
pub fn ensure_default_tag_taxonomy(frictions_root: &Path) -> Result<PathBuf, OrbitError> {
    let path = frictions_root.join(TAGS_FILENAME);
    if !path.exists() {
        atomic_write_text(&path, &default_taxonomy_body())
            .map_err(|error| OrbitError::from_write_io(&path, error))?;
        return Ok(path);
    }
    merge_missing_default_tags(&path)?;
    Ok(path)
}

fn default_taxonomy_body() -> String {
    let mut body = String::new();
    for (tag, description) in DEFAULT_FRICTION_TAGS {
        body.push_str(&format!("{tag}: \"{description}\"\n"));
    }
    body
}

fn merge_missing_default_tags(path: &Path) -> Result<(), OrbitError> {
    let raw = fs::read_to_string(path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let mut value: serde_yaml::Value = parse_yaml_with(&raw, path, |_, error| {
        OrbitError::InvalidInput(format!("parse {}: {error}", path.display()))
    })?;
    let mut tags = BTreeMap::new();
    collect_tag_descriptions_from_yaml(&value, &mut tags);
    if tags.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "{} must define at least one friction tag",
            path.display()
        )));
    }
    let mut missing = Vec::new();
    for &(tag, description) in DEFAULT_FRICTION_TAGS {
        if !tags.contains_key(tag) {
            missing.push((tag, description));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    insert_default_tags(&mut value, &missing, path)?;
    let serialized = serialize_yaml_with(&value, |error| {
        OrbitError::Store(format!("serialize {}: {error}", path.display()))
    })?;
    atomic_write_text(path, &serialized).map_err(|error| OrbitError::from_write_io(path, error))?;
    Ok(())
}

fn insert_default_tags(
    value: &mut serde_yaml::Value,
    missing: &[(&str, &str)],
    path: &Path,
) -> Result<(), OrbitError> {
    match value {
        serde_yaml::Value::Mapping(map) => {
            let tags_key = serde_yaml::Value::String("tags".to_string());
            if let Some(tags_value) = map.get_mut(&tags_key) {
                match tags_value {
                    serde_yaml::Value::Sequence(items) => {
                        for (tag, _) in missing {
                            items.push(serde_yaml::Value::String((*tag).to_string()));
                        }
                        Ok(())
                    }
                    serde_yaml::Value::Mapping(tag_map) => {
                        insert_tag_descriptions(tag_map, missing);
                        Ok(())
                    }
                    _ => Err(OrbitError::InvalidInput(format!(
                        "{} tags entry must be a list or mapping of tags",
                        path.display()
                    ))),
                }
            } else {
                insert_tag_descriptions(map, missing);
                Ok(())
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for (tag, _) in missing {
                items.push(serde_yaml::Value::String((*tag).to_string()));
            }
            Ok(())
        }
        _ => Err(OrbitError::InvalidInput(format!(
            "{} must define at least one friction tag",
            path.display()
        ))),
    }
}

fn insert_tag_descriptions(map: &mut serde_yaml::Mapping, missing: &[(&str, &str)]) {
    for (tag, description) in missing {
        map.insert(
            serde_yaml::Value::String((*tag).to_string()),
            serde_yaml::Value::String((*description).to_string()),
        );
    }
}

/// Load the configured tag names, seeding missing defaults.
pub fn load_tag_taxonomy(frictions_root: &Path) -> Result<BTreeSet<String>, OrbitError> {
    Ok(load_tag_taxonomy_with_descriptions(frictions_root)?
        .into_keys()
        .collect())
}

/// Read the workspace tag taxonomy. This compatibility path may seed or
/// merge defaults during reads, because existing workspaces can be missing
/// the file or predate a newly shipped tag. The taxonomy is configuration
/// on disk, separate from transactional friction records.
pub fn load_tag_taxonomy_with_descriptions(
    frictions_root: &Path,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let path = ensure_default_tag_taxonomy(frictions_root)?;
    let raw = fs::read_to_string(&path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let value: serde_yaml::Value = parse_yaml_with(&raw, &path, |_, error| {
        OrbitError::InvalidInput(format!("parse {}: {error}", path.display()))
    })?;
    let mut tags = BTreeMap::new();
    collect_tag_descriptions_from_yaml(&value, &mut tags);
    if tags.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "{} must define at least one friction tag",
            path.display()
        )));
    }
    Ok(tags)
}

fn collect_tag_descriptions_from_yaml(
    value: &serde_yaml::Value,
    out: &mut BTreeMap<String, String>,
) {
    const WORKSPACE_DEFINED_TAG: &str = "Workspace-defined friction category";
    match value {
        serde_yaml::Value::Mapping(map) => {
            if let Some(tags_value) = map.get(serde_yaml::Value::String("tags".to_string())) {
                collect_tag_descriptions_from_yaml(tags_value, out);
                return;
            }
            for (key, value) in map {
                if let Some(tag) = key.as_str().and_then(normalize_tag) {
                    let description = value
                        .as_str()
                        .map(str::trim)
                        .filter(|description| !description.is_empty())
                        .unwrap_or(WORKSPACE_DEFINED_TAG)
                        .to_string();
                    out.insert(tag, description);
                }
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                if let Some(tag) = item.as_str().and_then(normalize_tag) {
                    out.insert(tag, WORKSPACE_DEFINED_TAG.to_string());
                }
            }
        }
        _ => {}
    }
}

fn normalize_tag(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() { None } else { Some(value) }
}
