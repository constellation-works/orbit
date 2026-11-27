use orbit_common::Env;
use orbit_common::OrbitError;

use super::super::dispatch::ConfigTarget;
use super::common::ServerLaunch;
use super::simple_json::{apply_simple_json_init, apply_simple_json_remove};

pub(in crate::command::mcp::setup) fn apply_gemini_init(
    target: &ConfigTarget,
    launch: ServerLaunch<'_>,
    env: &Env,
) -> Result<(), OrbitError> {
    apply_simple_json_init(target, "mcpServers", launch, env)
}

pub(in crate::command::mcp::setup) fn apply_gemini_remove(
    target: &ConfigTarget,
    server_id: &str,
    env: &Env,
) -> Result<(), OrbitError> {
    apply_simple_json_remove(target, "mcpServers", server_id, env)
}
