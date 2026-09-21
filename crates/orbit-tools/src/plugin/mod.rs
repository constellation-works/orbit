//! Plugin loading: read a `plugin.yaml` v2 directory, resolve its schemas,
//! apply the namespace rules, and turn each declared tool into a registry
//! entry backed by the plugin's `exec` backend.
//!
//! Design: `docs/design/plugins/1_scope.md`. Lifecycle (install, enable,
//! pin file) lives in `orbit-core`; this module owns everything that needs a
//! plugin directory and a tool registry and nothing that needs a store.

mod loader;
mod migrate;
mod schema;
mod source;
mod tool;

#[cfg(test)]
mod tests;

pub use loader::{
    FIRST_PARTY_MANIFEST_DIGESTS, LoadedPlugin, PluginLoadError, PluginValidationPolicy,
    ResolvedPluginTool, first_party_source, load_plugin_dir, manifest_digest, manifest_refusal,
    validate_loaded_plugin,
};
pub use migrate::{SidecarManifest, load_sidecar_manifest, migrate_sidecars};
pub use schema::{input_schema_from_params, params_from_input_schema};
pub use source::{ResolvedSource, resolve_plugin_source};
pub use tool::{PLUGIN_ENVELOPE_SCHEMA_VERSION, PluginTool, PluginToolBinding};
