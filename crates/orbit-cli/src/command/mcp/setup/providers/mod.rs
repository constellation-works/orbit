mod claude;
mod common;
mod gemini;
mod simple_json;
mod toml_servers;

pub(super) use self::claude::{apply_claude_init, apply_claude_remove};
pub(super) use self::common::{ORBIT_FEDERATED_MCP_SERVER_ID, ServerLaunch};
pub(super) use self::gemini::{apply_gemini_init, apply_gemini_remove};
pub(super) use self::simple_json::{apply_simple_json_init, apply_simple_json_remove};
pub(super) use self::toml_servers::{apply_toml_init, apply_toml_remove};

#[cfg(test)]
mod tests;
