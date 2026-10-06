//! `plugin.yaml` v2: one manifest declaring a namespace and its tools.
//!
//! Every struct is `deny_unknown_fields` (the `RoutineDefinition` posture).
//! Every section is consumed: `tools` become the registry, CLI and MCP
//! surfaces, `definitions`/`skills`/`config` are installed on enable, `web`
//! feeds the dashboard's `plugins` group and `tests` drives `orbit plugin
//! test`.

mod cli_flags;
mod model;
mod validate;
mod web;

#[cfg(test)]
mod tests;

pub use cli_flags::{
    derive_plugin_cli_flag, plugin_root_in, validate_plugin_cli_flags,
    validate_plugin_cli_positionals,
};
pub use model::{
    MANIFEST_FILE_NAME, MANIFEST_KIND, MANIFEST_SCHEMA_VERSION, MAX_SECRET_NAME_LEN,
    PLUGIN_DIR_NAME, PluginBackend, PluginBackendType, PluginCliShape, PluginConfigSection,
    PluginDefinitions, PluginExecutionKind, PluginFsPermissions, PluginManifest,
    PluginManifestError, PluginMcpScope, PluginMetadata, PluginNetworkPermission, PluginOrigin,
    PluginPermissions, PluginRequires, PluginSandbox, PluginSecretSpec, PluginSpec, PluginToolSpec,
    is_valid_secret_name,
};
pub use validate::{plugin_provenance_label, validate_plugin_relative_path};
pub use web::{
    DEFAULT_PANEL_REFRESH_MS, LINK_URL_SCHEMES, MAX_PANEL_REFRESH_MS, MIN_PANEL_REFRESH_MS,
    PANEL_SOURCE_TOOL_PREFIX, PluginPanelGroup, PluginPanelRender, PluginWebLink, PluginWebPanel,
    PluginWebSection,
};
