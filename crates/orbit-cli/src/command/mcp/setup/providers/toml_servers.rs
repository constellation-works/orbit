//! Providers whose MCP servers live under `[mcp_servers.<id>]` in a TOML
//! config: Codex and Grok share the exact shape.

use orbit_core::OrbitError;
use toml_edit::{Array, Item, Table, value};

use super::super::dispatch::ConfigTarget;
use super::super::format::*;
use super::common::{ServerLaunch, server_args, server_id};

const SERVERS_KEY: &str = "mcp_servers";

pub(in crate::command::mcp::setup) fn apply_toml_init(
    target: &ConfigTarget,
    launch: ServerLaunch<'_>,
) -> Result<(), OrbitError> {
    let mut doc = load_toml_document(&target.mcp_path)?;
    ensure_toml_table(&mut doc, SERVERS_KEY)?
        .insert(server_id(launch), Item::Table(mcp_server_table(launch)));
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

pub(super) fn mcp_server_table(launch: ServerLaunch<'_>) -> Table {
    let mut table = Table::new();
    table.insert("command", value("orbit"));
    table.insert(
        "args",
        value(server_args(launch).into_iter().collect::<Array>()),
    );
    table.insert("enabled", value(true));
    table
}
