//! Plugin standard contracts: the `plugin.yaml` v2 manifest, the committed
//! `.orbit/plugins.yaml` pin file, the host-local installed-plugin record,
//! and the namespace rules every surface applies to plugin tool names.
//!
//! Design: `docs/design/plugins/1_scope.md`. This module holds data and pure
//! validation only; reading a plugin directory, resolving `$ref` targets, and
//! computing digests belong to `orbit-tools`.

mod grant;
mod manifest;
mod namespace;
mod pin;
mod record;
mod template;
mod version;

#[cfg(test)]
mod tests;

pub use grant::{PluginGrant, PluginGrantRequest, parse_grants};
pub use manifest::{
    MANIFEST_FILE_NAME, MANIFEST_KIND, MANIFEST_SCHEMA_VERSION, PluginBackend, PluginBackendType,
    PluginCliShape, PluginConfigSection, PluginDefinitions, PluginExecutionKind,
    PluginFsPermissions, PluginManifest, PluginManifestError, PluginMcpScope, PluginMetadata,
    PluginNetworkPermission, PluginOrigin, PluginPermissions, PluginRequires, PluginSandbox,
    PluginSpec, PluginToolSpec, plugin_provenance_label, validate_plugin_relative_path,
};
pub use namespace::{
    FIRST_PARTY_PUBLISHER, ORBIT_NAMESPACE_PREFIX, RESERVED_CLI_COMMANDS, is_valid_namespace,
    is_valid_verb, namespace_collides_with_tool, plugin_tool_name,
};
pub use pin::{PIN_FILE_NAME, PIN_FILE_SCHEMA_VERSION, PluginPin, PluginPinFile};
pub use record::{InstalledPlugin, PluginProvenance, PluginStatus};
pub use template::{
    PluginTemplateVars, is_allowed_template_reference, render_template, template_references,
    validate_template,
};
pub use version::{PLUGIN_HOST_API, SemverRange, Version, VersionError};
