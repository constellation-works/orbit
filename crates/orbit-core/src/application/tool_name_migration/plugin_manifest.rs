//! Rewriting pre-family tool names in plugin manifests, once per root, with
//! a digest record and rollback.

use std::fs;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_types::tool::family_action_tool_names;

use crate::OrbitRuntime;

use super::auto_task_catalog::{record_tool_name_rewrites, rewrite_yaml_list_tool_names};
#[cfg(any(test, feature = "test-support"))]
use super::plugin_manifest_publish_hook;
use super::plugin_recovery;
use super::{PLUGIN_TOOL_NAME_MIGRATION_MARKER, TOOL_NAME_MIGRATION_MARKER, ToolNameRewrite};

impl OrbitRuntime {
    /// Rewrite `permissions.orbit_tools` in each installed plugin manifest
    /// that names a pre-family operation, and record the rewritten digest.
    /// Only a manifest whose bytes still carry the digest recorded at install
    /// is rewritten: the rewrite names the same operations, so it keeps the
    /// operator's consent, while an unconsented edit stays refused.
    ///
    /// The digest is recorded after the file rewrite. If that record fails
    /// and the row still names the bytes this pass read, those bytes are put
    /// back and this pass returns the error, so the upgrade retries. If the
    /// row already names this rewrite, a competing pass finished it and the
    /// healed file is kept. A file already holding that rewrite — the record
    /// failed and the restore did not land — has its digest recorded on the
    /// next pass without another edit, using the digest-verified original
    /// retained before publication. Older writes without retained originals
    /// use bounded exact-digest recognition. The caller holds the pass's advisory
    /// lock across this work and the completion marker.
    pub(crate) fn migrate_plugin_manifest_tool_names(
        &self,
    ) -> Result<Vec<ToolNameRewrite>, OrbitError> {
        let global_root = self.global_root();
        let mut rewrites = Vec::new();
        for mut installed in self.stores().plugins().list_plugins()? {
            if crate::runtime::plugin::grants::verify_install_path(&global_root, &installed)
                .is_err()
            {
                continue;
            }
            let path =
                Path::new(&installed.install_path).join(orbit_types::plugin::MANIFEST_FILE_NAME);
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let file_digest = orbit_tools::plugin::manifest_digest(raw.as_bytes());
            if file_digest != installed.manifest_digest {
                // The file is already the consented rewrite and only the
                // stored digest is behind (the record failed after the
                // rewrite, and restoring the old bytes did not land).
                if let Some(rewrite) = plugin_recovery::consented_plugin_tool_rewrite(
                    &global_root,
                    &path,
                    &installed.name,
                    &raw,
                    &installed.manifest_digest,
                ) {
                    installed.manifest_digest = file_digest;
                    self.stores().plugins().upsert_plugin(&installed)?;
                    rewrites.push(rewrite);
                    continue;
                }
                // Another opener may have recorded the rewritten digest
                // before this opener's failed upsert restored the old bytes.
                // Reapply only when the stored digest proves this exact
                // rewrite is the one already recorded.
                if let Some((healed, rewrite)) =
                    plugin_manifest_tool_rewrite(&path, &installed.name, &raw)
                    && orbit_tools::plugin::manifest_digest(healed.as_bytes())
                        == installed.manifest_digest
                {
                    atomic_write_text(&path, &healed).map_err(|error| {
                        OrbitError::io_with_context(
                            &error,
                            format!("rewrite {}: {error}", path.display()),
                        )
                    })?;
                    rewrites.push(rewrite);
                }
                continue;
            }
            let Some((healed, rewrite)) =
                plugin_manifest_tool_rewrite(&path, &installed.name, &raw)
            else {
                continue;
            };
            let healed_digest = orbit_tools::plugin::manifest_digest(healed.as_bytes());
            let consented_digest = installed.manifest_digest.clone();
            // Commit the recovery evidence before either side of the pair
            // changes. A crash cannot strand a mixed-format manifest whose
            // original spelling is impossible to infer from the rewrite.
            plugin_recovery::retain_plugin_tool_original(&global_root, &raw)?;
            // Before this pass publishes, so a competing opener still reads
            // the consented bytes and can finish the rewrite itself.
            #[cfg(any(test, feature = "test-support"))]
            plugin_manifest_publish_hook::before_publish();
            atomic_write_text(&path, &healed).map_err(|error| {
                OrbitError::io_with_context(&error, format!("rewrite {}: {error}", path.display()))
            })?;
            installed.manifest_digest = healed_digest.clone();
            #[cfg(any(test, feature = "test-support"))]
            let recorded = if plugin_manifest_publish_hook::take_digest_upsert_failure() {
                Err(OrbitError::Store(
                    "injected plugin digest write failure".to_string(),
                ))
            } else {
                self.stores().plugins().upsert_plugin(&installed)
            };
            #[cfg(not(any(test, feature = "test-support")))]
            let recorded = self.stores().plugins().upsert_plugin(&installed);
            if let Err(error) = recorded {
                // The manifest and the stored digest have to stay a pair.
                // Restoring the consented bytes leaves the plugin loadable
                // when the row is still those bytes. A competing pass that
                // already recorded this rewrite must keep the healed file:
                // the completion marker would otherwise suppress the repair.
                self.settle_failed_plugin_digest_record(FailedPluginDigestRecord {
                    path: &path,
                    consented: &raw,
                    healed: &healed,
                    plugin_name: &installed.name,
                    healed_digest: &healed_digest,
                    consented_digest: &consented_digest,
                    error,
                })?;
                rewrites.push(rewrite);
                continue;
            }
            rewrites.push(rewrite);
        }
        record_tool_name_rewrites(&global_root.join("state"), &rewrites);
        Ok(rewrites)
    }

    /// Decide the file side of a digest record that did not land.
    ///
    /// `Ok` means a competing pass already recorded this exact rewrite, and
    /// the healed bytes are the manifest on disk. `Err` stops the pass: the
    /// consented bytes are back when the row still names them, and the file
    /// is left alone when the row moved somewhere else or could not be read.
    fn settle_failed_plugin_digest_record(
        &self,
        record: FailedPluginDigestRecord<'_>,
    ) -> Result<(), OrbitError> {
        let FailedPluginDigestRecord {
            path,
            consented,
            healed,
            plugin_name,
            healed_digest,
            consented_digest,
            error,
        } = record;
        let recorded = match self.stores().plugins().get_plugin(plugin_name) {
            Ok(plugin) => plugin.map(|plugin| plugin.manifest_digest),
            Err(read_error) => {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    plugin = plugin_name,
                    %error,
                    %read_error,
                    "left the rewritten plugin manifest in place because its stored digest could not be re-read"
                );
                return Err(error);
            }
        };
        if recorded.as_deref() == Some(healed_digest) {
            let on_disk_matches = fs::read(path)
                .is_ok_and(|bytes| orbit_tools::plugin::manifest_digest(&bytes) == healed_digest);
            if !on_disk_matches {
                atomic_write_text(path, healed).map_err(|io_error| {
                    OrbitError::io_with_context(
                        &io_error,
                        format!("keep rewritten {}: {io_error}", path.display()),
                    )
                })?;
            }
            tracing::debug!(
                target: "orbit.core.tool_names",
                plugin = plugin_name,
                "kept a plugin manifest rewrite a competing pass already recorded"
            );
            return Ok(());
        }
        if recorded.as_deref() == Some(consented_digest) {
            #[cfg(any(test, feature = "test-support"))]
            if plugin_manifest_publish_hook::take_restore_failure() {
                return Err(error);
            }
            if let Err(restore) = atomic_write_text(path, consented) {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    plugin = plugin_name,
                    %error,
                    %restore,
                    "plugin manifest rewrite could not be restored after its digest was not recorded"
                );
            }
            return Err(error);
        }
        tracing::warn!(
            target: "orbit.core.tool_names",
            plugin = plugin_name,
            %error,
            "left the rewritten plugin manifest in place because its stored digest changed"
        );
        Err(error)
    }
}

/// The file and the row a failed plugin-manifest digest record has to reconcile.
struct FailedPluginDigestRecord<'a> {
    path: &'a Path,
    consented: &'a str,
    healed: &'a str,
    plugin_name: &'a str,
    healed_digest: &'a str,
    consented_digest: &'a str,
    error: OrbitError,
}

/// One plugin-manifest upgrade pass, serialized with its completion marker.
///
/// The lock covers the marker check, the rewrite, the digest record, a
/// rollback and the marker publication. Another process cannot finish the
/// pass, or publish the marker, between this pass's read and its rollback.
/// The state directory is not created here: a root without one still has
/// nothing durable to mark, matching [`run_once`].
pub(super) fn run_plugin_manifest_migration_once(
    state_dir: &Path,
    pass: impl FnOnce() -> Result<(), OrbitError>,
) {
    const SCOPE: &str = "plugin manifests";
    if !std::fs::symlink_metadata(state_dir).is_ok_and(|meta| meta.is_dir()) {
        run_once(state_dir, PLUGIN_TOOL_NAME_MIGRATION_MARKER, SCOPE, pass);
        return;
    }
    let marker = state_dir.join(PLUGIN_TOOL_NAME_MIGRATION_MARKER);
    // Keep the v1 lock path so overlapping binaries still serialize.
    // The v2 marker re-runs the repair even if v1 marked a torn pair done.
    // A v2 marker records that this pass completed. Its check is repeated
    // inside the lock before any eligible rewrite.
    if marker.exists() {
        return;
    }
    if let Err(error) = with_exclusive_file_lock(
        &state_dir.join(TOOL_NAME_MIGRATION_MARKER),
        "plugin manifest tool-name migration",
        || -> std::io::Result<()> {
            run_once(state_dir, PLUGIN_TOOL_NAME_MIGRATION_MARKER, SCOPE, pass);
            Ok(())
        },
    ) {
        tracing::warn!(
            target: "orbit.core.tool_names",
            scope = SCOPE,
            %error,
            "pre-family plugin manifest migration could not take its lock; it retries on the next open"
        );
    }
}

pub(super) fn run_once(
    state_dir: &Path,
    marker_name: &str,
    scope: &str,
    pass: impl FnOnce() -> Result<(), OrbitError>,
) {
    let marker = state_dir.join(marker_name);
    if marker.exists() {
        return;
    }
    let result = pass().and_then(|()| {
        // A store with no state directory yet has nothing to migrate; the
        // open pass never creates (or follows a redirected) state root.
        if !std::fs::symlink_metadata(state_dir).is_ok_and(|meta| meta.is_dir()) {
            return Ok(());
        }
        atomic_write_text(&marker, &format!("{}\n", Utc::now().to_rfc3339()))
            .map_err(OrbitError::from)
    });
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.tool_names",
            scope,
            %error,
            "pre-family tool name migration did not complete; it retries on the next open"
        );
    }
}

/// Return the consent-preserving in-place rewrite of a plugin manifest when
/// the complete parsed manifest changes only by mapping its tool list.
pub(super) fn plugin_manifest_tool_rewrite(
    path: &Path,
    name: &str,
    raw: &str,
) -> Option<(String, ToolNameRewrite)> {
    let manifest = serde_yaml::from_str::<serde_yaml::Value>(raw).ok()?;
    let before = manifest_orbit_tools(&manifest)?;
    let after = family_action_tool_names(&before)?;
    let healed = rewrite_yaml_list_tool_names(raw, "orbit_tools");
    let healed_manifest = serde_yaml::from_str::<serde_yaml::Value>(&healed).ok();
    let mut expected = manifest;
    set_manifest_orbit_tools(&mut expected, &after);
    let rewritten_tools = healed_manifest.as_ref().and_then(manifest_orbit_tools);
    let mut healed_rest = healed_manifest;
    if let Some(value) = healed_rest.as_mut() {
        set_manifest_orbit_tools(value, &after);
    }
    if rewritten_tools
        .map(orbit_types::task::normalize_required_tools)
        .as_ref()
        != Some(&after)
        || healed_rest.as_ref() != Some(&expected)
    {
        tracing::warn!(
            target: "orbit.core.tool_names",
            plugin = name,
            "plugin manifest names pre-family tools in a layout that cannot be rewritten in place"
        );
        return None;
    }
    Some((
        healed,
        ToolNameRewrite {
            kind: "plugin",
            name: name.to_string(),
            path: path.to_path_buf(),
            before,
            after,
        },
    ))
}

fn manifest_orbit_tools(manifest: &serde_yaml::Value) -> Option<Vec<String>> {
    manifest
        .get("spec")?
        .get("permissions")?
        .get("orbit_tools")?
        .as_sequence()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.as_str().map(str::to_string))
                .collect()
        })
}

fn set_manifest_orbit_tools(manifest: &mut serde_yaml::Value, tools: &[String]) {
    if let Some(slot) = manifest
        .get_mut("spec")
        .and_then(|spec| spec.get_mut("permissions"))
        .and_then(|permissions| permissions.get_mut("orbit_tools"))
    {
        *slot = serde_yaml::Value::Sequence(
            tools
                .iter()
                .map(|tool| serde_yaml::Value::String(tool.clone()))
                .collect(),
        );
    }
}
