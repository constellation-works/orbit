use std::ffi::OsStr;
use std::path::Path;

use serde::Deserialize;

use orbit_common::OrbitError;

const DEFAULT_DOC_ROOT: &str = "docs/";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Deserialize)]
struct DocsConfigFile {
    docs: Option<DocsConfigSection>,
}

#[derive(Debug, Deserialize)]
struct DocsConfigSection {
    roots: Option<Vec<RawDocsRoot>>,
    search: Option<DocsSearchConfigSection>,
}

/// One `[docs] roots` entry. Either the plain path string (gitignore still
/// filters candidates under it, today's behavior) or a table naming the path
/// explicitly as authoritative over gitignore.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDocsRoot {
    Plain(String),
    Explicit {
        path: String,
        #[serde(default = "default_respect_gitignore")]
        respect_gitignore: bool,
    },
}

fn default_respect_gitignore() -> bool {
    true
}

impl From<RawDocsRoot> for DocsRoot {
    fn from(raw: RawDocsRoot) -> Self {
        match raw {
            RawDocsRoot::Plain(path) => DocsRoot {
                path,
                respect_gitignore: true,
            },
            RawDocsRoot::Explicit {
                path,
                respect_gitignore,
            } => DocsRoot {
                path,
                respect_gitignore,
            },
        }
    }
}

/// A configured `[docs].roots` entry, resolved to whether gitignore still
/// filters candidates found beneath it. Naming a root explicitly as a table
/// (`{ path = "...", respect_gitignore = false }`) opts it out of the
/// gitignore filter; a plain string keeps today's behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsRoot {
    pub path: String,
    pub respect_gitignore: bool,
}

impl DocsRoot {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            respect_gitignore: true,
        }
    }
}

impl From<&str> for DocsRoot {
    fn from(path: &str) -> Self {
        DocsRoot::new(path)
    }
}

impl From<String> for DocsRoot {
    fn from(path: String) -> Self {
        DocsRoot::new(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocsSearchConfig {
    pub semantic_weight: f32,
}

impl Default for DocsSearchConfig {
    fn default() -> Self {
        Self {
            semantic_weight: 0.5,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DocsSearchConfigSection {
    semantic_weight: Option<f32>,
}

pub fn parse_docs_roots_from_config_toml(raw: &str) -> Result<Vec<DocsRoot>, OrbitError> {
    if raw.trim().is_empty() {
        return Ok(default_doc_roots());
    }
    let parsed = toml::from_str::<DocsConfigFile>(raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid docs config in config.toml: {error}"))
    })?;
    Ok(parsed
        .docs
        .and_then(|section| section.roots)
        .map(|roots| roots.into_iter().map(DocsRoot::from).collect())
        .unwrap_or_else(default_doc_roots))
}

pub fn parse_docs_search_config_from_config_toml(
    raw: &str,
) -> Result<DocsSearchConfig, OrbitError> {
    if raw.trim().is_empty() {
        return Ok(DocsSearchConfig::default());
    }
    let parsed = toml::from_str::<DocsConfigFile>(raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid docs config in config.toml: {error}"))
    })?;
    let semantic_weight = parsed
        .docs
        .and_then(|section| section.search)
        .and_then(|section| section.semantic_weight)
        .unwrap_or_else(|| DocsSearchConfig::default().semantic_weight)
        .clamp(0.0, 1.0);
    Ok(DocsSearchConfig { semantic_weight })
}

pub(super) fn read_docs_roots_from_config_path(path: &Path) -> Result<Vec<DocsRoot>, OrbitError> {
    let Some(raw) = read_config_contents(path)? else {
        return Ok(default_doc_roots());
    };
    parse_docs_roots_from_config_toml(&raw)
}

pub(super) fn read_docs_search_config_from_config_path(
    path: &Path,
) -> Result<DocsSearchConfig, OrbitError> {
    let Some(raw) = read_config_contents(path)? else {
        return Ok(DocsSearchConfig::default());
    };
    parse_docs_search_config_from_config_toml(&raw)
}

pub(super) fn read_task_context_docs_roots_from_config_path(
    path: &Path,
) -> Result<Vec<DocsRoot>, OrbitError> {
    let Some(raw) = read_config_contents(path)? else {
        return Ok(default_doc_roots());
    };
    parse_task_context_docs_roots_from_config_toml(&raw)
}

fn read_config_contents(path: &Path) -> Result<Option<String>, OrbitError> {
    if !path.exists() {
        return Ok(None);
    }

    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "invalid config path without parent: {}",
            path.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize config directory {}: {error}",
            parent.display()
        ))
    })?;
    let canonical_path = path.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize config path {}: {error}",
            path.display()
        ))
    })?;
    if !canonical_path.starts_with(&canonical_parent)
        || canonical_path.file_name() != Some(OsStr::new(CONFIG_FILE_NAME))
        || !canonical_path.is_file()
    {
        return Err(OrbitError::InvalidInput(format!(
            "docs config path must be a regular {CONFIG_FILE_NAME} file: {}",
            path.display()
        )));
    }

    let safe_path = canonical_parent.join(CONFIG_FILE_NAME);
    let raw = std::fs::read_to_string(&safe_path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", safe_path.display())))?;
    Ok(Some(raw))
}

/// Parse the task-context docs roots (used by related_docs_for_task and its tests).
/// Visibility widened to pub(super) for ORB-00250 sibling tests/config.rs
/// (and the read_ wrapper in mod.rs calls it).
pub(super) fn parse_task_context_docs_roots_from_config_toml(
    raw: &str,
) -> Result<Vec<DocsRoot>, OrbitError> {
    if raw.trim().is_empty() {
        return Ok(default_doc_roots());
    }
    let parsed = toml::from_str::<DocsConfigFile>(raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid docs config in config.toml: {error}"))
    })?;
    Ok(match parsed.docs {
        Some(section) => section
            .roots
            .map(|roots| roots.into_iter().map(DocsRoot::from).collect())
            .unwrap_or_default(),
        None => default_doc_roots(),
    })
}

fn default_doc_roots() -> Vec<DocsRoot> {
    vec![DocsRoot::new(DEFAULT_DOC_ROOT)]
}
