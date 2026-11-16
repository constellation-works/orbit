//! Rewriting pre-family tool names in auto-task definitions and catalogs,
//! and the ledger each rewrite is recorded in.

use std::fs;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all};
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_types::tool::{family_action_tool_names, is_tool_family_operation};

use crate::application::auto_tasks::auto_tasks_dir;
use crate::application::auto_tasks::settings::AUTO_TASK_ASSET_KIND;
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, load_managed_asset_manifest,
    restore_managed_asset,
};

use super::{TOOL_NAME_MIGRATION_LEDGER, ToolNameRewrite};

/// Rewrite the operation names of the YAML list under `key` in `raw`: a block
/// sequence below the key or a flow sequence on its line. Everything else,
/// including comments and prose elsewhere, is left byte for byte. Callers
/// verify the parsed result, so a layout this does not recognize is refused
/// rather than half rewritten.
pub(super) fn rewrite_yaml_list_tool_names(raw: &str, key: &str) -> String {
    rewrite_yaml_keyed_list(raw, key, rewrite_line_tool_names)
}

pub(super) fn rewrite_yaml_keyed_list(
    raw: &str,
    key: &str,
    mut rewrite_line: impl FnMut(&str) -> String,
) -> String {
    let mut rewritten = String::with_capacity(raw.len());
    let mut list_indent: Option<usize> = None;
    for line in raw.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start();
        let indent = content.len() - trimmed.len();
        if let Some(key_indent) = list_indent {
            let continues = trimmed.is_empty()
                || trimmed.starts_with('#')
                || indent > key_indent
                || (indent == key_indent && trimmed.starts_with('-'));
            if !continues {
                list_indent = None;
            }
        }
        let is_key = trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'));
        if is_key {
            list_indent = Some(indent);
        }
        if list_indent.is_some() {
            rewritten.push_str(&rewrite_line(line));
        } else {
            rewritten.push_str(line);
        }
    }
    rewritten
}

/// Replace each operation name on `line` that stands as a whole list item:
/// after `-`, `[` or `,` and before `,`, `]`, `#` or the line end, optionally
/// quoted.
fn rewrite_line_tool_names(line: &str) -> String {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let mut rewritten = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(is_name_char) {
        let (before, tail) = rest.split_at(start);
        let end = tail.find(|c: char| !is_name_char(c)).unwrap_or(tail.len());
        let (token, after) = tail.split_at(end);
        rewritten.push_str(before);
        let opened = rewritten
            .trim_end()
            .trim_end_matches(['"', '\''])
            .trim_end()
            .ends_with(['-', '[', ',']);
        let closed = after
            .trim_start_matches(['"', '\''])
            .trim_start()
            .chars()
            .next()
            .is_none_or(|next| matches!(next, ',' | ']' | '#' | '\n' | '\r'));
        if opened && closed && is_tool_family_operation(token) {
            rewritten.push_str(&orbit_types::tool::tool_requirement_name(token));
        } else {
            rewritten.push_str(token);
        }
        rest = after;
    }
    rewritten.push_str(rest);
    rewritten
}

/// Rewrite one auto-task definition file whose `template.required_tools`
/// names a pre-family operation, and keep a tracked file managed. `None` when
/// the file needs nothing, cannot be parsed (doctor reports it faulty), or its
/// layout cannot be rewritten in place exactly (logged and left alone).
/// `write: false` reports the rewrite without making it.
///
/// The caller holds the workspace auto-task cursor lock when `write`.
pub(crate) fn heal_auto_task_definition_file(
    dir: &Path,
    path: &Path,
    write: bool,
) -> Result<Option<ToolNameRewrite>, OrbitError> {
    let raw = fs::read_to_string(path).map_err(|error| {
        OrbitError::io_with_context(&error, format!("read {}: {error}", path.display()))
    })?;
    let Ok(definition) = parse_auto_task_yaml(&raw) else {
        return Ok(None);
    };
    let Some(after) = family_action_tool_names(&definition.template.required_tools) else {
        return Ok(None);
    };
    let healed = rewrite_yaml_list_tool_names(&raw, "required_tools");
    let mut expected = definition.clone();
    expected.template.required_tools = after.clone();
    if parse_auto_task_yaml(&healed).ok().as_ref() != Some(&expected) {
        tracing::warn!(
            target: "orbit.core.tool_names",
            path = %path.display(),
            "auto-task definition names pre-family tools in a layout that cannot be rewritten \
             in place; admission still reads them as family actions"
        );
        return Ok(None);
    }
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(definition.name.as_str())
        .to_string();
    if write {
        let tracked = load_managed_asset_manifest(
            &dir.join(MANAGED_ASSET_MANIFEST_FILE),
            AUTO_TASK_ASSET_KIND,
            ManagedAssetLayout::YamlStem,
        )?
        .and_then(|manifest| manifest.assets.get(&name).cloned());
        atomic_write_text(path, &healed).map_err(|error| {
            OrbitError::io_with_context(&error, format!("rewrite {}: {error}", path.display()))
        })?;
        // The manifest proves Orbit wrote the old bytes; the rename is
        // Orbit's too, so the file stays managed rather than reading as a
        // fork.
        if tracked.as_deref() == Some(sha256_hex(raw.as_bytes()).as_str()) {
            restore_managed_asset(
                dir,
                AUTO_TASK_ASSET_KIND,
                ManagedAssetLayout::YamlStem,
                &name,
                sha256_hex(healed.as_bytes()),
            )?;
        }
    }
    Ok(Some(ToolNameRewrite {
        kind: "auto_task",
        name,
        path: path.to_path_buf(),
        before: definition.template.required_tools,
        after,
    }))
}

/// [`heal_auto_task_definition_file`] over every regular `*.yaml` file in
/// `<orbit_root>/auto_tasks`. A file that fails to read is logged and
/// skipped, so one bad file never stops the rest converging.
pub(crate) fn heal_auto_task_catalog(
    orbit_root: &Path,
    write: bool,
) -> Result<Vec<ToolNameRewrite>, OrbitError> {
    heal_auto_task_catalog_with_fork_policy(orbit_root, write, false)
}

/// Rewrite pre-family names during workspace sync while preserving edits to a
/// tracked definition. Upgrade and load paths use `heal_auto_task_catalog`
/// and still migrate every stored definition, including forks.
pub(crate) fn heal_workspace_sync_auto_task_catalog(
    orbit_root: &Path,
    write: bool,
) -> Result<Vec<ToolNameRewrite>, OrbitError> {
    heal_auto_task_catalog_with_fork_policy(orbit_root, write, true)
}

fn heal_auto_task_catalog_with_fork_policy(
    orbit_root: &Path,
    write: bool,
    preserve_managed_forks: bool,
) -> Result<Vec<ToolNameRewrite>, OrbitError> {
    let dir = auto_tasks_dir(orbit_root);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        _ => return Ok(Vec::new()),
    }
    let manifest = if preserve_managed_forks {
        load_managed_asset_manifest(
            &dir.join(MANAGED_ASSET_MANIFEST_FILE),
            AUTO_TASK_ASSET_KIND,
            ManagedAssetLayout::YamlStem,
        )?
    } else {
        None
    };
    let mut paths = fs::read_dir(&dir)
        .map_err(|error| {
            OrbitError::io_with_context(&error, format!("read {}: {error}", dir.display()))
        })?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml")
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut rewrites = Vec::new();
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let is_modified_managed_asset = manifest
            .as_ref()
            .and_then(|manifest| manifest.assets.get(name))
            .is_some_and(|expected_digest| {
                fs::read(&path)
                    .ok()
                    .is_some_and(|raw| sha256_hex(&raw) != *expected_digest)
            });
        if is_modified_managed_asset {
            continue;
        }
        match heal_auto_task_definition_file(&dir, &path, write) {
            Ok(Some(rewrite)) => rewrites.push(rewrite),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                target: "orbit.core.tool_names",
                path = %path.display(),
                %error,
                "skipped rewriting pre-family tool names in an auto-task definition"
            ),
        }
    }
    if write {
        record_tool_name_rewrites(&orbit_root.join("state"), &rewrites);
    }
    Ok(rewrites)
}

/// Append each rewrite to the root's ledger and log it. Recording is
/// bookkeeping: a failure is logged, never fails the rewrite it describes.
pub(crate) fn record_tool_name_rewrites(state_dir: &Path, rewrites: &[ToolNameRewrite]) {
    if rewrites.is_empty() {
        return;
    }
    for rewrite in rewrites {
        tracing::info!(
            target: "orbit.core.tool_names",
            kind = rewrite.kind,
            name = rewrite.name.as_str(),
            path = %rewrite.path.display(),
            before = ?rewrite.before,
            after = ?rewrite.after,
            "rewrote stored pre-family tool names to family actions"
        );
    }
    let ledger = state_dir.join(TOOL_NAME_MIGRATION_LEDGER);
    let at = Utc::now().to_rfc3339();
    let result = (|| {
        create_private_dir_all(state_dir)?;
        let mut content = match fs::read_to_string(&ledger) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        for rewrite in rewrites {
            let line = serde_json::json!({ "at": at, "rewrite": rewrite });
            content.push_str(&line.to_string());
            content.push('\n');
        }
        atomic_write_text(&ledger, &content)
    })();
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.tool_names",
            path = %ledger.display(),
            %error,
            "skipped recording tool name rewrites"
        );
    }
}
