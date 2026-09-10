//! Legacy file layout for friction records.
//!
//! ORB-10680 moved the live record store to SQLite
//! (the live friction repository). What remains here is the Markdown
//! layout itself: the reader the one-time import consumes, the writer the
//! export/inspection route re-materializes with, the tag taxonomy (a small
//! configuration file that did not move), and the hub publication helpers that
//! decide which tree a workspace's legacy evidence lives in.
//!
//! Nothing in this module is on a live read or write path. Legacy trees are
//! read-only rollback evidence for one release; no code here deletes them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_common::governance::friction::DEFAULT_FRICTION_TAGS;
use orbit_types::record::{FrictionFrontmatter, FrictionRecord};

use crate::contracts::StoredFrictionRecord;
use crate::fs::yaml::{parse_yaml_with, serialize_yaml_with};

const TAGS_FILENAME: &str = "tags.yaml";
const HUB_FRICTION_MIGRATION_MARKERS: &str = ".migration-markers";

/// Canonical checkout-independent friction state for a logical workspace.
pub fn canonical_hub_friction_root(
    global_root: &Path,
    workspace_id: &str,
) -> Result<PathBuf, OrbitError> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty()
        || workspace_id == "."
        || workspace_id == ".."
        || workspace_id.contains(['/', '\\'])
    {
        return Err(OrbitError::InvalidInput(format!(
            "invalid logical workspace ID '{workspace_id}' for hub friction state"
        )));
    }
    Ok(global_root
        .join("frictions")
        .join("workspaces")
        .join(workspace_id))
}

/// Publishes legacy checkout-local friction state into the canonical hub root.
///
/// The destination directory is renamed into place only after a complete copy.
/// The separate marker is the commit record: reads continue using `legacy_root`
/// until it exists. Identical interrupted/repeated publishes are idempotent;
/// differing source and destination trees fail closed.
pub fn prepare_hub_friction_root(
    global_root: &Path,
    workspace_id: &str,
    legacy_root: Option<&Path>,
) -> Result<PathBuf, OrbitError> {
    let canonical = canonical_hub_friction_root(global_root, workspace_id)?;
    let parent = canonical.parent().ok_or_else(|| {
        OrbitError::Store("canonical hub friction root has no parent".to_string())
    })?;
    let marker = parent
        .join(HUB_FRICTION_MIGRATION_MARKERS)
        .join(format!("{workspace_id}.complete"));
    let lock = parent
        .join(HUB_FRICTION_MIGRATION_MARKERS)
        .join(format!("{workspace_id}.migration"));
    with_exclusive_file_lock(&lock, "hub friction migration", || {
        fs::create_dir_all(parent).map_err(|error| OrbitError::Io(error.to_string()))?;
        if marker.exists() {
            if !canonical.is_dir() {
                return Err(OrbitError::Store(format!(
                    "hub friction migration marker for workspace '{workspace_id}' exists but canonical root '{}' is unavailable",
                    canonical.display()
                )));
            }
            return Ok(canonical.clone());
        }

        // A checkoutless caller may not know whether legacy state exists.
        // Make the canonical root usable, but do not publish a migration
        // decision that would prevent a later caller with the legacy root
        // from copying or conflict-checking that state.
        if legacy_root.is_none() {
            fs::create_dir_all(&canonical).map_err(|error| OrbitError::Io(error.to_string()))?;
            return Ok(canonical.clone());
        }

        let source = legacy_root.filter(|path| path.exists());
        if canonical.exists()
            && let Some(source) = source
            && !directory_trees_identical(source, &canonical)?
        {
            if directory_tree_is_empty(&canonical)? {
                fs::remove_dir_all(&canonical)
                    .map_err(|error| OrbitError::Io(error.to_string()))?;
            } else {
                return Err(OrbitError::Store(format!(
                    "hub friction migration conflict for workspace '{workspace_id}': legacy '{}' differs from uncommitted canonical '{}'",
                    source.display(),
                    canonical.display()
                )));
            }
        }
        if !canonical.exists()
            && let Some(source) = source
        {
            let staging = parent.join(format!(
                ".{workspace_id}.migration-{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            if staging.exists() {
                fs::remove_dir_all(&staging).map_err(|error| OrbitError::Io(error.to_string()))?;
            }
            if let Err(error) = copy_directory_tree(source, &staging) {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
            fs::rename(&staging, &canonical).map_err(|error| {
                let _ = fs::remove_dir_all(&staging);
                OrbitError::Io(format!(
                    "publish hub friction migration '{}': {error}",
                    canonical.display()
                ))
            })?;
        } else if !canonical.exists() {
            fs::create_dir_all(&canonical).map_err(|error| OrbitError::Io(error.to_string()))?;
        }

        atomic_write_text(&marker, "schema_version: 1\nstate: complete\n")?;
        Ok(canonical.clone())
    })
}

fn directory_tree_is_empty(root: &Path) -> Result<bool, OrbitError> {
    for entry in fs::read_dir(root).map_err(|error| OrbitError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        if file_type.is_file() || !file_type.is_dir() || !directory_tree_is_empty(&entry.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Resolves the readable root without treating an unmarked publish as committed.
pub fn readable_hub_friction_root(
    global_root: &Path,
    workspace_id: &str,
    legacy_root: Option<&Path>,
) -> Result<PathBuf, OrbitError> {
    let canonical = canonical_hub_friction_root(global_root, workspace_id)?;
    let parent = canonical.parent().ok_or_else(|| {
        OrbitError::Store("canonical hub friction root has no parent".to_string())
    })?;
    let marker = parent
        .join(HUB_FRICTION_MIGRATION_MARKERS)
        .join(format!("{workspace_id}.complete"));
    if !marker.exists()
        && let Some(legacy) = legacy_root.filter(|path| path.exists())
    {
        return Ok(legacy.to_path_buf());
    }
    Ok(canonical)
}

fn copy_directory_tree(source: &Path, destination: &Path) -> Result<(), OrbitError> {
    fs::create_dir_all(destination).map_err(|error| OrbitError::Io(error.to_string()))?;
    for entry in fs::read_dir(source).map_err(|error| OrbitError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target).map_err(|error| OrbitError::Io(error.to_string()))?;
        } else {
            return Err(OrbitError::Store(format!(
                "hub friction migration refuses non-file entry '{}'",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn directory_trees_identical(left: &Path, right: &Path) -> Result<bool, OrbitError> {
    fn snapshot(
        root: &Path,
        current: &Path,
        directories: &mut BTreeSet<PathBuf>,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<(), OrbitError> {
        for entry in fs::read_dir(current).map_err(|error| OrbitError::Io(error.to_string()))? {
            let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|error| OrbitError::Io(error.to_string()))?;
            if file_type.is_dir() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| OrbitError::Store(error.to_string()))?
                    .to_path_buf();
                directories.insert(relative);
                snapshot(root, &entry.path(), directories, files)?;
            } else if file_type.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| OrbitError::Store(error.to_string()))?
                    .to_path_buf();
                files.insert(
                    relative,
                    fs::read(entry.path()).map_err(|error| OrbitError::Io(error.to_string()))?,
                );
            } else {
                return Err(OrbitError::Store(format!(
                    "hub friction migration refuses non-file entry '{}'",
                    entry.path().display()
                )));
            }
        }
        Ok(())
    }
    let mut left_directories = BTreeSet::new();
    let mut right_directories = BTreeSet::new();
    let mut left_files = BTreeMap::new();
    let mut right_files = BTreeMap::new();
    snapshot(left, left, &mut left_directories, &mut left_files)?;
    snapshot(right, right, &mut right_directories, &mut right_files)?;
    Ok(left_directories == right_directories && left_files == right_files)
}

pub fn ensure_default_tag_taxonomy(frictions_root: &Path) -> Result<PathBuf, OrbitError> {
    let path = frictions_root.join(TAGS_FILENAME);
    if !path.exists() {
        let mut body = String::new();
        for (tag, description) in DEFAULT_FRICTION_TAGS {
            body.push_str(&format!("{tag}: \"{description}\"\n"));
        }
        atomic_write_text(&path, &body).map_err(|error| OrbitError::from_write_io(&path, error))?;
    }
    Ok(path)
}

pub(crate) fn load_tag_taxonomy(frictions_root: &Path) -> Result<BTreeSet<String>, OrbitError> {
    let path = ensure_default_tag_taxonomy(frictions_root)?;
    let raw = fs::read_to_string(&path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let value: serde_yaml::Value = parse_yaml_with(&raw, &path, |_, error| {
        OrbitError::InvalidInput(format!("parse {}: {error}", path.display()))
    })?;
    let mut tags = BTreeSet::new();
    collect_tags_from_yaml(&value, &mut tags);
    if tags.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "{} must define at least one friction tag",
            path.display()
        )));
    }
    Ok(tags)
}

fn collect_tags_from_yaml(value: &serde_yaml::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            if let Some(tags_value) = map.get(serde_yaml::Value::String("tags".to_string())) {
                collect_tags_from_yaml(tags_value, out);
                return;
            }
            for key in map.keys() {
                if let Some(tag) = key.as_str().and_then(normalize_tag) {
                    out.insert(tag);
                }
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                if let Some(tag) = item.as_str().and_then(normalize_tag) {
                    out.insert(tag);
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

pub(crate) fn write_record_at(path: &Path, record: &FrictionRecord) -> Result<(), OrbitError> {
    let frontmatter = FrictionFrontmatter {
        id: record.id.clone(),
        title: record.title.clone(),
        model: record.model.clone(),
        created_at: record.created_at,
        status: record.status,
        tags: record.tags.clone(),
        resolved_at: record.resolved_at,
        during_task: record.during_task.clone(),
        resolved_by_task: record.resolved_by_task.clone(),
    };
    let yaml = serialize_yaml_with(&frontmatter, |error| {
        OrbitError::Store(format!("serialize friction frontmatter: {error}"))
    })?;
    let content = format!("---\n{}---\n{}\n", yaml, record.body.trim_end());
    atomic_write_text(path, &content).map_err(|error| OrbitError::from_write_io(path, error))?;
    Ok(())
}

pub(crate) fn read_record_at(path: &Path) -> Result<StoredFrictionRecord, OrbitError> {
    let raw = fs::read_to_string(path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let (yaml, body) = split_frontmatter(&raw).ok_or_else(|| {
        OrbitError::Store(format!(
            "friction record {} must start with YAML frontmatter",
            path.display()
        ))
    })?;
    let frontmatter: FrictionFrontmatter = parse_yaml_with(yaml, path, |_, error| {
        OrbitError::Store(format!(
            "parse friction frontmatter {}: {error}",
            path.display()
        ))
    })?;
    Ok(StoredFrictionRecord {
        record: FrictionRecord {
            id: frontmatter.id,
            title: frontmatter.title,
            model: frontmatter.model,
            created_at: frontmatter.created_at,
            status: frontmatter.status,
            tags: frontmatter.tags,
            resolved_at: frontmatter.resolved_at,
            during_task: frontmatter.during_task,
            resolved_by_task: frontmatter.resolved_by_task,
            body: body.trim_start_matches('\n').trim_end().to_string(),
        },
        path: Some(path.to_path_buf()),
    })
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---\n")?;
    let (yaml, body) = rest.split_once("\n---\n")?;
    Some((yaml, body))
}

/// Resolve the legacy corpus root before enumerating records.
///
/// The root is selected by the workspace configuration, but it is still a
/// path boundary for the importer. Rejecting a symlinked root and returning
/// its canonical directory prevents a configured path from redirecting the
/// import outside the selected legacy tree.
pub(crate) fn validated_friction_root(frictions_root: &Path) -> Result<PathBuf, OrbitError> {
    let metadata = fs::symlink_metadata(frictions_root).map_err(|error| {
        OrbitError::Io(format!(
            "inspect friction corpus root {}: {error}",
            frictions_root.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "friction corpus root must be a regular directory: {}",
            frictions_root.display()
        )));
    }

    let canonical_root = fs::canonicalize(frictions_root).map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize friction corpus root {}: {error}",
            frictions_root.display()
        ))
    })?;
    if !canonical_root.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "canonical friction corpus root must be a directory: {}",
            canonical_root.display()
        )));
    }

    Ok(canonical_root)
}

pub(crate) fn friction_record_paths(frictions_root: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let frictions_root = validated_friction_root(frictions_root)?;
    let mut paths = Vec::new();
    for month_entry in fs::read_dir(&frictions_root)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", frictions_root.display())))?
    {
        let month_entry = month_entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        let month_type = month_entry
            .file_type()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        if !month_type.is_dir() {
            continue;
        }
        let month_path = month_entry.path();
        for record_entry in fs::read_dir(&month_path)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", month_path.display())))?
        {
            let record_entry = record_entry.map_err(|error| OrbitError::Io(error.to_string()))?;
            let record_type = record_entry
                .file_type()
                .map_err(|error| OrbitError::Io(error.to_string()))?;
            let path = record_entry.path();
            if record_type.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("md")
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests;
