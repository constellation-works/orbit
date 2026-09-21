//! Plugin loading and execution: read a `plugin.yaml` v2 directory, resolve
//! its schemas, apply the namespace rules, and turn each declared tool into
//! a registry entry backed by the plugin's `exec` or `mcp` backend running
//! under the granted sandbox profile.
//!
//! Design: `docs/design/plugins/1_scope.md`. Lifecycle (install, enable,
//! grants, pin file) lives in `orbit-core`; this module owns everything that
//! needs a plugin directory and a tool registry and nothing that needs a
//! store.

mod backend;
mod envelope;
mod loader;
mod mcp;
mod migrate;
mod schema;
mod source;
mod tool;

#[cfg(test)]
mod tests;

pub use backend::{PLUGIN_TIMEOUT_CEILING_MS, PluginBackendSpec, PluginSandboxProfile};
pub use envelope::{PLUGIN_ENVELOPE_SCHEMA_VERSION, parse_response, validate_output};
pub use loader::{
    FIRST_PARTY_MANIFEST_DIGESTS, LoadedPlugin, PluginLoadError, PluginValidationPolicy,
    ResolvedPluginTool, first_party_source, load_plugin_dir, manifest_digest, manifest_refusal,
    validate_loaded_plugin,
};
pub use mcp::{McpBackend, McpExpectedTool};
pub use migrate::{SidecarManifest, load_sidecar_manifest, migrate_sidecars};
pub use schema::{input_schema_from_params, params_from_input_schema};
pub use source::{ResolvedSource, resolve_plugin_source};
pub use tool::{PluginBackend, PluginTool, PluginToolBinding};
