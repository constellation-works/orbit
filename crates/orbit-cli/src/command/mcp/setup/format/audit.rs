//! Evidence emitted at the actual file mutation, including runtime-free init.

use std::panic::Location;
use std::path::Path;

/// Record only Orbit launch arguments, never the client's other servers,
/// settings, or environment. The tracing layer redacts invocation arguments.
#[track_caller]
pub(super) fn record_config_write(
    path: &Path,
    target: &Path,
    action: &str,
    servers: serde_json::Value,
) {
    tracing::info!(
        target: "orbit::mcp_config",
        event = "mcp_config_write",
        path = %path.display(),
        resolved_path = %target.display(),
        caller = %Location::caller(),
        action,
        pid = std::process::id(),
        run_id = std::env::var("ORBIT_RUN_ID").ok().as_deref(),
        activity_id = std::env::var("ORBIT_ACTIVITY_ID").ok().as_deref(),
        cwd = ?std::env::current_dir().ok(),
        args = ?std::env::args_os().collect::<Vec<_>>(),
        server_args = %servers,
        "MCP client config changed"
    );
}

pub(super) fn orbit_server_args(root: &serde_json::Value) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    for key in ["mcpServers", "servers", "mcp_servers"] {
        for id in ["orbit", "orbit-federated"] {
            if let Some(value) = root
                .get(key)
                .and_then(|servers| servers.get(id))
                .and_then(|entry| entry.get("args"))
            {
                args.insert(id.to_string(), value.clone());
            }
        }
    }
    serde_json::Value::Object(args)
}
