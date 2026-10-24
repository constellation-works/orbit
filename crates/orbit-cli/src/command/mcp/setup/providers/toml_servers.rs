//! Providers whose MCP servers live under `[mcp_servers.<id>]` in a TOML
//! config: Codex and Grok share the exact shape.

use orbit_common::security::child_env::MCP_MANAGED_BINDING_ENV_VARS;
use orbit_core::OrbitError;
use toml_edit::{Array, Item, Table, value};

use super::super::dispatch::ConfigTarget;
use super::super::format::*;
use super::common::{ServerLaunch, missing_root_args, preserve_operator, server_args, server_id};

const SERVERS_KEY: &str = "mcp_servers";

pub(in crate::command::mcp::setup) fn apply_toml_init(
    target: &ConfigTarget,
    launch: ServerLaunch<'_>,
    codex: bool,
) -> Result<(), OrbitError> {
    let mut doc = load_toml_document(&target.mcp_path)?;
    let preserved = doc
        .get(SERVERS_KEY)
        .and_then(|servers| servers.get(server_id(launch)))
        .and_then(|entry| entry.get("args"))
        .and_then(Item::as_array)
        .is_some_and(|args| {
            preserve_operator(launch, args.iter().filter_map(toml_edit::Value::as_str))
        });
    if preserved {
        if supply_toml_root(&mut doc, launch) {
            return write_toml_document(&target.mcp_path, &doc);
        }
        return Ok(());
    }
    let mut table = mcp_server_table(launch);
    if codex && matches!(launch, ServerLaunch::Local { .. }) {
        table.insert(
            "env_vars",
            value(
                MCP_MANAGED_BINDING_ENV_VARS
                    .iter()
                    .copied()
                    .collect::<Array>(),
            ),
        );
    }
    ensure_toml_table(&mut doc, SERVERS_KEY)?.insert(server_id(launch), Item::Table(table));
    write_toml_document(&target.mcp_path, &doc)
}

pub(in crate::command::mcp::setup) fn apply_toml_remove(
    target: &ConfigTarget,
    server_id: &str,
) -> Result<(), OrbitError> {
    let mut doc = load_toml_document(&target.mcp_path)?;
    if let Some(servers) = doc.get_mut(SERVERS_KEY).and_then(Item::as_table_like_mut) {
        servers.remove(server_id);
        if servers.is_empty() {
            doc.remove(SERVERS_KEY);
        }
    }
    write_or_remove_toml_document(&target.mcp_path, &doc)
}

/// Insert the selected catalog root into a preserved operator entry.
///
/// Returns whether the document changed. An entry that already names a root,
/// through argv or `ORBIT_ROOT`, is left as the operator wrote it.
fn supply_toml_root(doc: &mut toml_edit::DocumentMut, launch: ServerLaunch<'_>) -> bool {
    let server_id = server_id(launch);
    let env_root = doc
        .get(SERVERS_KEY)
        .and_then(|servers| servers.get(server_id))
        .and_then(|entry| entry.get("env"))
        .and_then(|env| env.get("ORBIT_ROOT"))
        .and_then(Item::as_str)
        .map(str::to_string);
    let Some(args) = doc
        .get_mut(SERVERS_KEY)
        .and_then(Item::as_table_like_mut)
        .and_then(|servers| servers.get_mut(server_id))
        .and_then(|entry| entry.get_mut("args"))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    if args.iter().any(|value| value.as_str().is_none()) {
        return false;
    }
    let prefix = {
        let current = args
            .iter()
            .filter_map(toml_edit::Value::as_str)
            .collect::<Vec<_>>();
        missing_root_args(current, env_root.as_deref(), launch)
    };
    let Some([flag, root]) = prefix else {
        return false;
    };
    args.insert(0, root);
    args.insert(0, flag);
    true
}

fn mcp_server_table(launch: ServerLaunch<'_>) -> Table {
    let mut table = Table::new();
    table.insert("command", value("orbit"));
    table.insert(
        "args",
        value(server_args(launch).into_iter().collect::<Array>()),
    );
    table.insert("enabled", value(true));
    table
}
