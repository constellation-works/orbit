//! Comment-preserving edits to provider TOML configs (`~/.codex/config.toml`,
//! `~/.grok/config.toml`). These are the user's own files, so only Orbit's
//! server entry changes; comments, key order, and formatting survive.

use std::fs;
use std::path::Path;

use orbit_common::fs::io::atomic_write_text;
use orbit_core::OrbitError;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use super::audit::{orbit_server_args, record_config_write};
use super::target::{is_config_symlink, resolve_config_target};

pub(in crate::command::mcp::setup) fn load_toml_document(
    path: &Path,
) -> Result<DocumentMut, OrbitError> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let raw = fs::read_to_string(path)
        .map_err(|err| OrbitError::Io(format!("failed to read '{}': {err}", path.display())))?;
    raw.parse::<DocumentMut>().map_err(|err| {
        OrbitError::InvalidInput(format!("invalid TOML '{}': {err}", path.display()))
    })
}

/// Write `doc` atomically, so a failed write leaves the previous config
/// intact. A config symlinked from a dotfiles repository stays a symlink: the
/// file it resolves to is the one replaced.
#[track_caller]
pub(in crate::command::mcp::setup) fn write_toml_document(
    path: &Path,
    doc: &DocumentMut,
) -> Result<(), OrbitError> {
    let target = resolve_config_target(path)?;
    let rendered = doc.to_string();
    if fs::read_to_string(&target).is_ok_and(|current| current == rendered) {
        return Ok(());
    }
    let parsed: toml::Value = toml::from_str(&rendered).map_err(|err| {
        OrbitError::InvalidInput(format!("invalid TOML '{}': {err}", path.display()))
    })?;
    let root = serde_json::to_value(parsed).map_err(|err| {
        OrbitError::Execution(format!("serialize TOML '{}': {err}", path.display()))
    })?;
    atomic_write_text(&target, &rendered).map_err(|err| {
        OrbitError::Io(format!(
            "failed to atomically write '{}': {err}",
            path.display()
        ))
    })?;
    record_config_write(path, &target, "write", orbit_server_args(&root));
    Ok(())
}

/// Like [`write_toml_document`], but delete the file once nothing but
/// whitespace would remain. A symlinked config is kept, and its target is
/// emptied instead; a dangling link is left as it is.
#[track_caller]
pub(in crate::command::mcp::setup) fn write_or_remove_toml_document(
    path: &Path,
    doc: &DocumentMut,
) -> Result<(), OrbitError> {
    if doc.to_string().trim().is_empty() {
        if !path.exists() {
            return Ok(());
        }
        if is_config_symlink(path) {
            return write_toml_document(path, doc);
        }
        fs::remove_file(path).map_err(|err| {
            OrbitError::Io(format!("failed to remove '{}': {err}", path.display()))
        })?;
        record_config_write(path, path, "remove", serde_json::json!({}));
        return Ok(());
    }
    write_toml_document(path, doc)
}

/// The table at top-level `key`, created (as an implicit `[key.*]` parent)
/// when absent. An inline table is accepted as-is.
pub(in crate::command::mcp::setup) fn ensure_toml_table<'a>(
    doc: &'a mut DocumentMut,
    key: &str,
) -> Result<&'a mut dyn TableLike, OrbitError> {
    let item = doc.entry(key).or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    item.as_table_like_mut()
        .ok_or_else(|| OrbitError::InvalidInput(format!("expected '{key}' to be a TOML table")))
}
