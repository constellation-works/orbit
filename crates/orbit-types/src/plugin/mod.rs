//! Plugin standard contracts: the `plugin.yaml` v2 manifest, the committed
//! `.orbit/plugins.yaml` pin file, the host-local installed-plugin record,
//! and the namespace rules every surface applies to plugin tool names.
//!
//! Design: `docs/design/plugins/1_scope.md`. This module holds data and pure
//! validation only; reading a plugin directory, resolving `$ref` targets, and
//! computing digests belong to `orbit-tools`.

mod build;
mod conformance;
mod error;
mod grant;
mod manifest;
mod namespace;
mod pin;
mod record;
mod template;
mod version;

#[cfg(test)]
mod tests;

pub use build::{
    BUILD_DIR_TEMPLATE, DEFAULT_PLUGIN_BUILD_TIMEOUT_MS, PLUGIN_BUILD_CONSENT_FLAG,
    PLUGIN_BUILD_DIGEST_DOMAIN, PLUGIN_BUILD_PROFILE_LINUX, PLUGIN_BUILD_PROFILE_MACOS,
    PLUGIN_BUILD_TIMEOUT_CEILING_MS, PluginBuildConsent, PluginBuildOutput,
    PluginBuildOutputRecord, PluginBuildProgram, PluginBuildRecord, PluginBuildSpec,
    artifact_digest_preimage, format_plugin_build_argv, git_commit_source, is_full_commit_id,
    render_build_argv,
};
pub use conformance::{
    FIXTURE_SECRET_VERSION, PluginTestCase, PluginTestErrorExpectation, PluginTestExpectation,
    PluginTestFile, TEST_FILE_KIND, TEST_FILE_SCHEMA_VERSION,
};
pub use error::{ArchiveDigestError, PluginGrantError, PluginPinError};
pub use grant::{
    PluginGrant, PluginGrantEntry, PluginGrantRequest, PluginGrantSet, parse_grant_entries,
    parse_grants, parse_stored_grants, resolve_grant_selection,
};
pub use manifest::{
    DEFAULT_PANEL_REFRESH_MS, LINK_URL_SCHEMES, MANIFEST_FILE_NAME, MANIFEST_KIND,
    MANIFEST_SCHEMA_VERSION, MAX_PANEL_REFRESH_MS, MAX_SECRET_NAME_LEN, MIN_PANEL_REFRESH_MS,
    PANEL_SOURCE_TOOL_PREFIX, PLUGIN_DIR_NAME, PluginBackend, PluginBackendType, PluginCliShape,
    PluginConfigSection, PluginDefinitions, PluginExecutionKind, PluginFsPermissions,
    PluginManifest, PluginManifestError, PluginMcpScope, PluginMetadata, PluginNetworkPermission,
    PluginOrigin, PluginPanelGroup, PluginPanelRender, PluginPermissions, PluginRequires,
    PluginSandbox, PluginSecretSpec, PluginSpec, PluginToolSpec, PluginWebLink, PluginWebPanel,
    PluginWebSection, derive_plugin_cli_flag, is_valid_secret_name, plugin_provenance_label,
    plugin_root_in, validate_plugin_cli_flags, validate_plugin_cli_positionals,
    validate_plugin_relative_path,
};
pub use namespace::{
    FIRST_PARTY_PUBLISHER, ORBIT_NAMESPACE_PREFIX, RESERVED_CLI_COMMANDS, is_valid_namespace,
    is_valid_verb, namespace_collides_with_tool, plugin_tool_name,
};
pub use pin::{
    PIN_FILE_NAME, PIN_FILE_SCHEMA_VERSION, PLUGIN_ARCHIVE_EXTENSIONS, PluginPin, PluginPinFile,
    parse_archive_digest, remote_archive_source,
};
pub use record::{
    InstalledPlugin, PluginDisabledLayer, PluginProvenance, PluginSecretUpdateStatus, PluginStatus,
};
pub use template::{
    PluginTemplateVars, is_allowed_template_reference, render_template, template_references,
    validate_template,
};
pub use version::{PLUGIN_HOST_API, SemverRange, Version, VersionError};
