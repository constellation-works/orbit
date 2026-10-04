//! `orbit plugin remove`.

use orbit_common::OrbitError;
use orbit_types::plugin::is_valid_namespace;
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::application::plugin::build::build_log_path;
use crate::runtime::plugin::build_witness::forget_build_witness;
use crate::runtime::plugin::grants::forget_authorized_grants;
use crate::runtime::plugin::paths::{plugin_namespace_dir, plugin_state_dir};
use crate::runtime::plugin::sandbox_mask::{not_visible, plugin_trees_masked};

use super::super::install::lock_plugin_namespace;
use super::super::secrets::delete_plugin_secrets;
use super::disable::unlink_namespace_skills;
use super::record::{installed_plugin, set_enabled, verified_install_path};

/// What `orbit plugin remove` was asked to delete.
#[derive(Debug, Clone, Default)]
pub struct PluginRemoveOptions {
    /// Drop this host's record of the plugin — its `plugins` row, its grant
    /// witness and its discovery links — and leave every file under the
    /// install root where it is.
    ///
    /// The recovery path for a row whose `install_path` this host cannot
    /// verify: ordinary removal deletes the recorded tree and therefore
    /// refuses such a row, which would otherwise leave the operator with a
    /// refused plugin and no command that removes it [ORB-12800].
    pub record_only: bool,
    /// Delete this namespace's Orbit-owned state tree as well as its install.
    pub purge_state: bool,
}

/// Remove the host's install, optionally including its Orbit-owned state.
/// Derived data a plugin wrote outside that state tree is retained (§3).
///
/// Only this namespace's install family is deleted. The recorded
/// `install_path` is as writable as the rest of the row, so it is verified
/// against the namespace install directory before anything is removed; a row
/// that fails the check is refused whole, before any mutation, and
/// [`PluginRemoveOptions::record_only`] is what clears it.
///
/// The whole removal holds the namespace lock `add` and `upgrade` take, and
/// reads the row only once it has it: an overlapping install either lands
/// first and is removed whole, or starts after and installs into an empty
/// namespace.
pub fn remove_plugin(
    runtime: &OrbitRuntime,
    name: &str,
    options: &PluginRemoveOptions,
) -> Result<(), OrbitError> {
    if options.record_only && options.purge_state {
        return Err(OrbitError::InvalidInput(
            "--purge-state cannot be combined with --record-only".to_string(),
        ));
    }
    // An ordinary removal deletes the plugin's secrets, and `--purge-state`
    // its state. Inside an agent sandbox both trees are masked, so either
    // would find nothing there and report it gone; refuse before any change.
    if !options.record_only && plugin_trees_masked(&runtime.global_root()) {
        return Err(not_visible("this plugin's state and secrets"));
    }
    let _namespace_lock = lock_plugin_namespace(&runtime.global_root(), name)?;
    let installed = installed_plugin(runtime, name)?;
    if !options.record_only && !is_valid_namespace(name) {
        return Err(OrbitError::PolicyDenied(format!(
            "refusing to remove plugin with invalid namespace '{name}'; use --record-only to clear its record"
        )));
    }
    // Ordinary removal deletes files, so the recorded path is held to this
    // host's install directory first; `--record-only` is the verb for a row
    // that cannot pass.
    let owns_install = if options.record_only {
        false
    } else {
        verified_install_path(runtime, &installed)?;
        true
    };
    let state_dir = plugin_state_dir(&runtime.global_root(), name);
    if options.purge_state {
        // A plugin may write inside its own state tree. Refuse links at any
        // state-path component before changing the plugin row or install.
        for path in [
            runtime.global_root().join("state"),
            runtime.global_root().join("state/plugins"),
            state_dir.clone(),
        ] {
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(OrbitError::PolicyDenied(format!(
                        "refusing to purge plugin state through symlinked path {}",
                        path.display()
                    )));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(OrbitError::PolicyDenied(format!(
                        "refusing to purge plugin state through non-directory path {}",
                        path.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(OrbitError::Io(format!(
                        "inspect {}: {error}",
                        path.display()
                    )));
                }
            }
        }
    }

    // Take the plugin off the surface before the record goes. Besides
    // stopping its tools and seeded definitions from registering, this removes
    // every discovery link into this namespace's install family; deleting the
    // tree first would leave those links dangling with no plugin row left for
    // doctor to inspect.
    set_enabled(runtime, name, false, None, None)?;
    unlink_namespace_skills(runtime, name)?;

    // Keep the row available for a retry if state removal fails. All path
    // checks above ran before the first lifecycle mutation.
    if options.purge_state && state_dir.exists() {
        std::fs::remove_dir_all(&state_dir)
            .map_err(|error| OrbitError::Io(format!("remove {}: {error}", state_dir.display())))?;
    }
    // The plugin's secrets go with its install, before the row, so a failure
    // leaves the row for a retry. `--record-only` removes nothing but the
    // record, so the secrets stay for a reinstall or `orbit plugin secret rm`.
    if !options.record_only {
        delete_plugin_secrets(&runtime.global_root(), name)?;
    }

    runtime.with_mutation(|| {
        runtime.stores().plugins().delete_plugin(name)?;
        Ok((
            (),
            OrbitEvent::PluginRemoved {
                name: name.to_string(),
            },
        ))
    })?;

    // The authority goes with the install: a later reinstall of this namespace
    // starts from no authorized grants rather than inheriting these.
    forget_authorized_grants(&runtime.global_root(), name);
    forget_build_witness(&runtime.global_root(), name);

    // Everything below deletes files, so it runs only for a verified install.
    if !owns_install {
        return Ok(());
    }
    // The whole namespace family goes, not only the recorded version. An
    // upgrade performed by an Orbit that did not prune may have left older
    // `<ns>/<version>/` trees, and every plugin backend can read the install
    // family (§4.3), so leaving them would leave this plugin's code on the
    // host after `remove` reported it gone. Deleting the directory rather than
    // the recorded path also retires the previous `remove_dir(parent)`, which
    // silently failed whenever anything else was still in there. The path is
    // derived from the namespace and the global root, never from the row
    // [ORB-12800], and the verification above already held the recorded path
    // to it.
    let namespace_dir = plugin_namespace_dir(&runtime.global_root(), name);
    if namespace_dir.exists() {
        std::fs::remove_dir_all(&namespace_dir).map_err(|error| {
            OrbitError::Io(format!("remove {}: {error}", namespace_dir.display()))
        })?;
    }
    // The build log describes a tree that no longer exists.
    if let Some(log_dir) = build_log_path(&runtime.global_root(), name).parent()
        && log_dir.exists()
    {
        std::fs::remove_dir_all(log_dir)
            .map_err(|error| OrbitError::Io(format!("remove {}: {error}", log_dir.display())))?;
    }
    Ok(())
}
