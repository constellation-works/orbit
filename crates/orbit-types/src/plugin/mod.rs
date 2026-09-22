//! Plugin standard contracts: the `plugin.yaml` v2 manifest, the committed
//! `.orbit/plugins.yaml` pin file, the host-local installed-plugin record,
//! and the namespace rules every surface applies to plugin tool names.
//!
//! Design: `docs/design/plugins/1_scope.md`. This module holds data and pure
//! validation only; reading a plugin directory, resolving `$ref` targets, and
//! computing digests belong to `orbit-tools`.

mod conformance;
mod grant;
mod manifest;
mod namespace;
mod pin;
mod record;
mod template;
mod version;

#[cfg(test)]
mod tests;

pub use conformance::{
    PluginTestCase, PluginTestExpectation, PluginTestFile, TEST_FILE_KIND, TEST_FILE_SCHEMA_VERSION,
};
pub use grant::{
    PluginGrant, PluginGrantRequest, parse_grants, parse_stored_grants, resolve_grant_selection,
};
pub use manifest::{
    DEFAULT_PANEL_REFRESH_MS, LINK_URL_SCHEMES, MANIFEST_FILE_NAME, MANIFEST_KIND,
    MANIFEST_SCHEMA_VERSION, MAX_PANEL_REFRESH_MS, MIN_PANEL_REFRESH_MS, PANEL_SOURCE_TOOL_PREFIX,
    PluginBackend, PluginBackendType, PluginCliShape, PluginConfigSection, PluginDefinitions,
    PluginExecutionKind, PluginFsPermissions, PluginManifest, PluginManifestError, PluginMcpScope,
    PluginMetadata, PluginNetworkPermission, PluginOrigin, PluginPanelGroup, PluginPanelRender,
    PluginPermissions, PluginRequires, PluginSandbox, PluginSpec, PluginToolSpec, PluginWebLink,
    PluginWebPanel, PluginWebSection, derive_plugin_cli_flag, plugin_provenance_label,
    validate_plugin_cli_flags, validate_plugin_cli_positionals, validate_plugin_relative_path,
};
pub use namespace::{
    FIRST_PARTY_PUBLISHER, ORBIT_NAMESPACE_PREFIX, RESERVED_CLI_COMMANDS, is_valid_namespace,
    is_valid_verb, namespace_collides_with_tool, plugin_tool_name,
};
pub use pin::{
    PIN_FILE_NAME, PIN_FILE_SCHEMA_VERSION, PLUGIN_ARCHIVE_EXTENSIONS, PluginPin, PluginPinFile,
    parse_archive_digest, remote_archive_source,
};
pub use record::{InstalledPlugin, PluginProvenance, PluginStatus};
pub use template::{
    PluginTemplateVars, is_allowed_template_reference, render_template, template_references,
    validate_template,
};
pub use version::{PLUGIN_HOST_API, SemverRange, Version, VersionError};
