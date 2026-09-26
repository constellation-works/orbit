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
mod callback;
mod envelope;
mod loader;
mod mcp;
mod migrate;
mod schema;
mod source;
mod tool;

#[cfg(test)]
mod tests;

pub use backend::{
    DeliveredPluginSecret, DroppedFsRoot, PLUGIN_GRANT_WITNESS_DIR, PLUGIN_SECRET_STORE_DIR,
    PLUGIN_TIMEOUT_CEILING_MS, PluginBackendSpec, PluginConfigSection, PluginProgramStatus,
    PluginSandboxProfile, PluginSecretDelivery, PluginSecretRotation, PluginSecretSource,
    RenderedFsRoots, plugin_grant_witness_relative, program_statuses, render_fs_roots,
    resolve_declared_programs,
};
pub use callback::{
    CallbackResolution, ORBIT_PLUGIN_CALLBACK_ENV, ORBIT_PLUGIN_CALLBACK_FD_ENV, ORBIT_PLUGIN_ENV,
    PLUGIN_CALLBACK_FD, PluginCallbackIdentity, PluginCallbackSession, invalid_callback_credential,
    mismatched_callback_credential, resolve_plugin_callback, resolve_plugin_callback_session,
    retired_callback_credential, stale_plugin_callback_session_count, unidentified_plugin_child,
};
pub use envelope::{
    PLUGIN_ENVELOPE_SCHEMA_VERSION, parse_response, take_delivered_plugin_secret_names,
    take_plugin_secret_updates, validate_output,
};
pub use loader::{
    FIRST_PARTY_MANIFEST_DIGESTS, LoadedPlugin, LoadedPluginTestFile, PluginDefinitionFiles,
    PluginLoadError, PluginValidationPolicy, ResolvedPluginTool, first_party_source,
    fs_write_root_covers, load_plugin_dir, manifest_digest, manifest_refusal,
    physical_with_missing_tail, plugin_symlink_refusal, refuse_covering_fs_write_roots,
    refuse_plugin_tree_symlinks, validate_loaded_plugin,
};
pub use mcp::{McpBackend, McpExpectedTool};
pub use migrate::{SidecarManifest, load_sidecar_manifest, migrate_sidecars};
pub use schema::{CompiledSchema, input_schema_from_params, params_from_input_schema};
pub use source::{PluginSourceRequest, ResolvedSource, resolve_plugin_source};
pub use tool::{PluginBackend, PluginTool, PluginToolBinding};
