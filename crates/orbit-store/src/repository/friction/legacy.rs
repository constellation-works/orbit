//! Decode the legacy Markdown framing and YAML row for one imported friction.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::record::{FrictionFrontmatter, FrictionRecord};

use crate::contract::StoredFrictionRecord;
use crate::driver::file::friction_store::{LegacyFrictionDir, friction_record_paths};
use crate::fs::yaml::parse_yaml_with;

/// Read the legacy records through the one-shot import's file snapshot.
pub(crate) struct LegacyFrictionRepository<'a> {
    layout: &'a LegacyFrictionDir,
}

impl<'a> LegacyFrictionRepository<'a> {
    pub(crate) fn new(layout: &'a LegacyFrictionDir) -> Self {
        Self { layout }
    }

    pub(crate) fn paths(&self) -> Result<Vec<PathBuf>, OrbitError> {
        friction_record_paths(&self.layout.root)
    }

    pub(crate) fn get(&self, path: &Path) -> Result<StoredFrictionRecord, OrbitError> {
        read_record_at(path)
    }
}

fn read_record_at(path: &Path) -> Result<StoredFrictionRecord, OrbitError> {
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
            rehome_to: frontmatter.rehome_to,
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
