use std::fs;
use std::path::Path;

use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_core::OrbitError;
use orbit_types::tool::mcp_advertised_tool_name;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::command::mcp::{ORBIT_MCP_SERVER_ID, safe_mcp_tool_names};

use super::super::args::ScopeArg;
use super::super::dispatch::ConfigTarget;
use super::super::format::*;
use super::common::{ServerLaunch, server_args, server_id};

pub(in crate::command::mcp::setup) fn apply_claude_init(
    target: &ConfigTarget,
    launch: ServerLaunch<'_>,
) -> Result<(), OrbitError> {
    let server_id = server_id(launch);
    cleanup_legacy_mcp_path(target, server_id)?;

    let update_mcp = || {
        let mut root = load_json_object(&target.mcp_path)?;
        let mcp_servers = ensure_json_object(&mut root, "mcpServers")?;
        mcp_servers.insert(server_id.to_string(), claude_mcp_server_value(launch));
        if target.scope == ScopeArg::Home {
            write_json_object_atomic(&target.mcp_path, &root)
        } else {
            write_json_object(&target.mcp_path, &root)
        }
    };
    if target.scope == ScopeArg::Home {
        with_exclusive_file_lock(&target.mcp_path, "Claude Code main settings", update_mcp)?;
    } else {
        update_mcp()?;
    }

    if let Some(settings_path) = &target.settings_path {
        let mut settings = load_json_object(settings_path)?;
        let permissions = ensure_json_object(&mut settings, "permissions")?;
        let allow = ensure_json_string_array(permissions, "allow")?;
        merge_unique_strings(allow, claude_safe_permissions(server_id));
        write_json_object(settings_path, &settings)?;
    }
    Ok(())
}

pub(in crate::command::mcp::setup) fn apply_claude_remove(
    target: &ConfigTarget,
    server_id: &str,
) -> Result<(), OrbitError> {
    let remove_mcp = || {
        let mut root = load_json_object(&target.mcp_path)?;
        if let Some(mcp_servers) = root
            .get_mut("mcpServers")
            .and_then(JsonValue::as_object_mut)
        {
            mcp_servers.remove(server_id);
            if mcp_servers.is_empty() {
                root.remove("mcpServers");
            }
        }
        if target.scope == ScopeArg::Home {
            if target.mcp_path.exists() {
                write_json_object_atomic(&target.mcp_path, &root)?;
            }
            Ok(())
        } else {
            write_or_remove_json_object(&target.mcp_path, &root)
        }
    };
    if target.scope == ScopeArg::Home {
        with_exclusive_file_lock(&target.mcp_path, "Claude Code main settings", remove_mcp)?;
    } else {
        remove_mcp()?;
    }
    cleanup_legacy_mcp_path(target, server_id)?;

    if let Some(settings_path) = &target.settings_path {
        let mut settings = load_json_object(settings_path)?;
        let mut remove_keys = Vec::new();
        if let Some(permissions) = settings
            .get_mut("permissions")
            .and_then(JsonValue::as_object_mut)
        {
            if let Some(allow) = permissions
                .get_mut("allow")
                .and_then(JsonValue::as_array_mut)
            {
                remove_known_strings(allow, &claude_safe_permissions(server_id));
                // Migration cleanup: prior `orbit mcp init --claude` runs wrote
                // plugin-scoped names (`mcp__plugin_orbit_orbit__*`) that the
                // current init no longer produces. Strip them here so a single
                // `orbit mcp remove --claude` after upgrade leaves a clean
                // settings.json instead of orphaning stale entries.
                if server_id == ORBIT_MCP_SERVER_ID {
                    remove_known_strings(allow, &claude_legacy_safe_permissions());
                }
                if allow.is_empty() {
                    permissions.remove("allow");
                }
            }
            if permissions.is_empty() {
                remove_keys.push("permissions".to_string());
            }
        }
        for key in remove_keys {
            settings.remove(&key);
        }
        write_or_remove_json_object(settings_path, &settings)?;

        if let Some(settings_dir) = settings_path.parent() {
            remove_dir_if_empty(settings_dir)?;
        }
    }
    Ok(())
}

fn cleanup_legacy_mcp_path(target: &ConfigTarget, server_id: &str) -> Result<(), OrbitError> {
    let Some(legacy_path) = &target.legacy_mcp_path else {
        return Ok(());
    };

    let mut root = load_json_object(legacy_path)?;
    if let Some(mcp_servers) = root
        .get_mut("mcpServers")
        .and_then(JsonValue::as_object_mut)
    {
        mcp_servers.remove(server_id);
        if mcp_servers.is_empty() {
            root.remove("mcpServers");
        }
    }
    write_or_remove_json_object(legacy_path, &root)
}

/// Remove `dir` if it exists and is now empty.
///
/// `apply_claude_init` calls `write_json_object`, which `create_dir_all`s the
/// settings file's parent on demand — for a workspace or home root with no
/// prior `.claude/`, that is this directory. A clean `remove` should leave the
/// tree as it found it, so once the settings file this function owns is gone,
/// an empty directory is one `remove` itself created and should go with it.
/// Any remaining content (a user's `commands/`, unrelated settings, …) means
/// this directory predates `init` or serves another purpose, so it is left
/// alone.
fn remove_dir_if_empty(dir: &Path) -> Result<(), OrbitError> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)
        .map_err(|err| OrbitError::Io(format!("failed to read '{}': {err}", dir.display())))?;
    if entries.next().is_some() {
        return Ok(());
    }
    fs::remove_dir(dir)
        .map_err(|err| OrbitError::Io(format!("failed to remove '{}': {err}", dir.display())))
}

pub(super) fn claude_mcp_server_value(launch: ServerLaunch<'_>) -> JsonValue {
    JsonValue::Object(JsonMap::from_iter([
        (
            "command".to_string(),
            JsonValue::String("orbit".to_string()),
        ),
        (
            "args".to_string(),
            JsonValue::Array(
                server_args(launch)
                    .into_iter()
                    .map(JsonValue::String)
                    .collect(),
            ),
        ),
    ]))
}

fn claude_safe_permissions(server_id: &str) -> Vec<String> {
    safe_mcp_tool_names()
        .into_iter()
        .map(|name| claude_permission_name_for_server(server_id, &name))
        .collect()
}

#[cfg(test)]
pub(super) fn claude_permission_name(tool_name: &str) -> String {
    claude_permission_name_for_server(ORBIT_MCP_SERVER_ID, tool_name)
}

fn claude_permission_name_for_server(server_id: &str, tool_name: &str) -> String {
    // pub(super) widened so providers/tests/claude.rs can call it
    // (sibling under providers per ORB-00221 layout).
    //
    // Claude derives MCP permission names from the connected server id in
    // .mcp.json. The v1 server registers as `orbit`, while the opt-in mux uses
    // `orbit-federated`; permission entries must follow the actual server id
    // so the two integrations can coexist.
    // The plugin-scoped shape `mcp__plugin_<plugin>_<server>__<tool>` is
    // what Claude synthesized for Orbit's retired extension distribution.
    // That path did not run this code, so the plugin-scoped prefix is
    // intentionally not emitted from the CLI registration path.
    // See `claude_legacy_safe_permissions` for the one-shot cleanup of
    // stale plugin-prefixed entries left by pre-ORB-00286 CLI runs.
    format!(
        "mcp__{}__{}",
        server_id,
        mcp_advertised_tool_name(tool_name)
    )
}

fn claude_legacy_safe_permissions() -> Vec<String> {
    // Pre-ORB-00286 the CLI emitted `mcp__plugin_orbit_orbit__<tool>`
    // entries — the plugin-scoped shape that Claude Code synthesizes for
    // its *plugin* install path, not for bare `.mcp.json` registrations.
    // Existing users carry these stale entries in their settings.json;
    // `apply_claude_remove` strips them alongside the current
    // `claude_safe_permissions(ORBIT_MCP_SERVER_ID)` so an upgrade + `orbit mcp remove
    // --claude` leaves a clean file. Keep the generator independent from
    // `claude_permission_name` so a future prefix change doesn't break
    // this migration.
    safe_mcp_tool_names()
        .into_iter()
        .map(|name| {
            format!(
                "mcp__plugin_orbit_orbit__{}",
                mcp_advertised_tool_name(&name)
            )
        })
        .collect()
}
