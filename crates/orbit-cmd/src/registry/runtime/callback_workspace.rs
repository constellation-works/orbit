//! CLI callback preflight, before any selected workspace store opens.
use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::resolve_plugin_callback;

use super::RegisteredRuntimeFactory;
use super::selection::canonical_or_original;

/// Refuse a plugin callback selecting a checkout outside its backend sandbox.
///
/// This reads the host-issued credential and registry only. It must run before
/// bootstrap, since opening the selected store can itself require migrations.
/// Older live sessions cannot prove a workspace binding, so explicit selectors
/// are refused until their backend restarts.
pub fn refuse_cli_callback_workspace(
    global_root: &Path,
    tool: &str,
    selector: &str,
) -> Result<(), OrbitError> {
    let Some(identity) = resolve_plugin_callback(global_root)? else {
        return Ok(());
    };
    if identity.unconfined {
        return Ok(());
    }
    let selected = RegisteredRuntimeFactory::resolve_workspace_selector(global_root, selector)?;
    let bound = identity.workspace_root.as_ref().and_then(|root| {
        RegisteredRuntimeFactory::resolve_workspace_selector(global_root, &root.to_string_lossy())
            .ok()
    });
    if bound.as_ref().is_some_and(|bound| {
        canonical_or_original(&bound.checkout.orbit_dir)
            == canonical_or_original(&selected.checkout.orbit_dir)
    }) {
        return Ok(());
    }
    let reason = if identity.workspace_root.is_none() {
        "the callback session has no recorded sandbox workspace; restart the plugin backend before using an explicit workspace selector"
    } else {
        "the checkout is outside this plugin backend's sandbox; callbacks can use only the workspace the backend is bound to"
    };
    let error = OrbitError::CallbackWorkspaceOutsideSandbox(format!(
        "workspace selector '{selector}' refused: {reason}"
    ));
    tracing::warn!(plugin = %identity.provenance.name, tool, workspace = selector,
        code = "callback_workspace_outside_sandbox", "{error}");
    Err(error)
}
