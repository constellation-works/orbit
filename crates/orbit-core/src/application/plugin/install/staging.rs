//! Refusing an in-repository source, and staging a tree for an atomic swap.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockGuard, FileLockOptions, acquire_exclusive_file_lock, try_acquire_exclusive_file_lock,
};
use orbit_tools::plugin::{
    is_live_plugin_build_dir, plugin_symlink_refusal, refuse_plugin_tree_symlinks,
};

use crate::OrbitRuntime;
use crate::runtime::plugin::grants::plugin_grant_witness_path;
use crate::runtime::plugin::paths::plugin_namespace_dir;

/// Global install only (§3): a plugin tree inside the repository would be
/// vendored state the workspace must not carry.
pub(super) fn refuse_in_repository_source(
    runtime: &OrbitRuntime,
    source_root: &Path,
) -> Result<(), OrbitError> {
    let repo_root = runtime.paths().repo_root.clone();
    let Ok(repo_root) = std::fs::canonicalize(&repo_root) else {
        return Ok(());
    };
    if !source_root.starts_with(&repo_root) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "refusing to install '{}': it is inside the repository at {}. Orbit plugins are \
         global-install-only — the host installs once under `~/.orbit/plugins/` and a \
         checkout only pins them in `.orbit/plugins.yaml`, so a plugin tree is never vendored \
         into a checkout. Move the plugin outside the repository and add it from there, or \
         pin it in `.orbit/plugins.yaml` and run `orbit plugin sync`.",
        source_root.display(),
        repo_root.display()
    )))
}

/// A plugin tree staged beside the version directories, and the tree it
/// replaces.
///
/// `add --force` used to delete the live `<version>/` and copy the new tree
/// into it file by file, so a concurrent `orbit` — a clock tick, an MCP
/// server, a dashboard panel — could load a `plugin.yaml` that was already in
/// place while `bin/backend` was still being written, and execute truncated
/// bytes. The copy now lands in a staging directory in the same namespace
/// directory and becomes visible with a single `rename`, so a reader sees
/// either the whole old tree or the whole new one. Replacing a tree does leave
/// a brief moment with no `<version>/` at all, between renaming the old one
/// aside and renaming the new one in: `rename` cannot replace a non-empty
/// directory, and a reader that lands there gets a plain "not installed"
/// error rather than half a plugin.
///
/// The swap rolls back unless [`Self::commit`] is reached. If restoration
/// fails, the displaced tree stays at a reported recovery path rather than
/// being deleted while the old row still names it.
pub(super) struct StagedInstall {
    staging: PathBuf,
    install_path: PathBuf,
    /// Where the replaced tree was moved, held until the row names the new one.
    displaced: Option<PathBuf>,
    published: bool,
    #[cfg(unix)]
    published_identity: Option<(u64, u64)>,
    committed: bool,
}

impl StagedInstall {
    pub(super) fn begin(global_root: &Path, name: &str, version: &str) -> Result<Self, OrbitError> {
        let namespace_dir = plugin_namespace_dir(global_root, name);
        std::fs::create_dir_all(&namespace_dir).map_err(|error| {
            OrbitError::Io(format!("create {}: {error}", namespace_dir.display()))
        })?;
        Ok(Self {
            staging: namespace_dir.join(scratch_name("staging")),
            install_path: namespace_dir.join(version),
            displaced: None,
            published: false,
            #[cfg(unix)]
            published_identity: None,
            committed: false,
        })
    }

    /// Where the tree is copied before it is anything a reader can reach.
    pub(super) fn staging(&self) -> &Path {
        &self.staging
    }

    /// Move any tree already at `<version>/` aside, then make the staged one
    /// visible with one rename.
    pub(super) fn publish(&mut self) -> Result<(), OrbitError> {
        self.publish_with(|_, _| {})
    }

    /// The callback gives fault-path tests a deterministic point between the
    /// two renames; production passes a no-op.
    pub(super) fn publish_with(
        &mut self,
        after_displacement: impl FnOnce(&Path, &Path),
    ) -> Result<(), OrbitError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.published_identity = self
                .staging
                .symlink_metadata()
                .ok()
                .map(|metadata| (metadata.dev(), metadata.ino()));
        }
        if self.install_path.symlink_metadata().is_ok() {
            let displaced = self.install_path.with_file_name(scratch_name("replaced"));
            std::fs::rename(&self.install_path, &displaced).map_err(|error| {
                OrbitError::Io(format!("replace {}: {error}", self.install_path.display()))
            })?;
            self.displaced = Some(displaced);
        }
        after_displacement(&self.staging, &self.install_path);
        if let Err(error) = std::fs::rename(&self.staging, &self.install_path) {
            let failure = format!("install {}: {error}", self.install_path.display());
            return Err(match self.rollback() {
                Ok(()) => OrbitError::Io(failure),
                Err(rollback_error) => OrbitError::Io(format!("{failure}; {rollback_error}")),
            });
        }
        self.published = true;
        Ok(())
    }

    /// Undo either side of the publish boundary. If another actor occupies
    /// the live path, keep both that path and the displaced tree untouched.
    pub(super) fn rollback(&mut self) -> Result<(), OrbitError> {
        if self.published {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let owned = self.published_identity.is_some_and(|identity| {
                    self.install_path
                        .symlink_metadata()
                        .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == identity)
                });
                if !owned {
                    return Err(self.recovery_error("the published path changed"));
                }
            }
            std::fs::remove_dir_all(&self.install_path)
                .map_err(|error| self.recovery_error(&format!("remove published tree: {error}")))?;
            self.published = false;
        }
        if let Some(displaced) = self.displaced.as_ref() {
            if self.install_path.symlink_metadata().is_ok() {
                return Err(self.recovery_error("the live path is occupied"));
            }
            std::fs::rename(displaced, &self.install_path)
                .map_err(|error| self.recovery_error(&format!("restore previous tree: {error}")))?;
            self.displaced = None;
        }
        Ok(())
    }

    fn recovery_error(&self, reason: &str) -> OrbitError {
        let message = if let Some(displaced) = self.displaced.as_ref() {
            format!(
                "could not restore the previous plugin installation ({reason}); recover its bytes at {}",
                displaced.display()
            )
        } else {
            format!(
                "could not roll back the plugin installation ({reason}); inspect {}",
                self.install_path.display()
            )
        };
        OrbitError::Io(message)
    }

    /// The row names the staged tree: keep it, and drop the replaced one.
    pub(super) fn commit(&mut self) {
        self.committed = true;
        if let Some(displaced) = self.displaced.take() {
            remove_install_scratch(&displaced);
        }
    }
}

impl Drop for StagedInstall {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Err(error) = self.rollback() {
            tracing::warn!(
                target: "orbit.core.plugin",
                "a failed install could not put the replaced plugin tree back: {error}",
            );
        }
        remove_install_scratch(&self.staging);
    }
}

/// A name inside the namespace directory that only this install owns. The
/// leading dot cannot collide with a version directory: a plugin version is
/// semver, which never starts with one.
fn scratch_name(kind: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (seconds, nanos) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or((0, 0), |since| (since.as_secs(), since.subsec_nanos()));
    format!(
        ".{kind}-{pid}-{seconds:x}{nanos:x}-{nonce:x}",
        pid = std::process::id(),
        nonce = COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

/// Delete a leftover the install owns. Best effort: the caller is either
/// unwinding from an error it will report, or finishing an install that has
/// already landed.
fn remove_install_scratch(path: &Path) {
    let Ok(metadata) = path.symlink_metadata() else {
        return;
    };
    let removed = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(error) = removed {
        tracing::warn!(
            target: "orbit.core.plugin",
            path = %path.display(),
            "left behind a plugin install directory that could not be removed: {error}",
        );
    }
}

/// Hold the one lock that serializes every change to `name`'s install family,
/// `plugins` row and grant witness: `add`, `upgrade` and `remove`.
///
/// Without it, two installs of different versions could each prune the
/// other's tree, leaving a row that names nothing, and a `remove` could delete
/// the namespace directory under an install that was still staging into it.
/// The lock is per namespace, so unrelated plugins still install in parallel;
/// resolving and fetching a source happens before it is taken.
///
/// The lock file sits beside the namespace's witness: outside the namespace
/// directory, which `remove` deletes whole, and in a tree no plugin backend
/// can read, so no plugin can open it to hold an install off.
pub(in crate::application::plugin) fn lock_plugin_namespace(
    global_root: &Path,
    name: &str,
) -> Result<FileLockGuard, OrbitError> {
    const LABEL: &str = "plugin namespace";
    let path = plugin_namespace_lock_path(global_root, name);
    let lock_error =
        |error: std::io::Error| OrbitError::Io(format!("lock {}: {error}", path.display()));
    if let Some(guard) = try_acquire_exclusive_file_lock(&path, LABEL).map_err(lock_error)? {
        return Ok(guard);
    }
    tracing::info!(
        target: "orbit.core.plugin",
        plugin = %name,
        "waiting for another add, upgrade or remove of this plugin to finish",
    );
    acquire_exclusive_file_lock(&path, LABEL, FileLockOptions::default()).map_err(lock_error)
}

/// Where [`lock_plugin_namespace`] locks. An invalid name maps to the
/// witness's reserved leaf, so it never becomes a path component.
fn plugin_namespace_lock_path(global_root: &Path, name: &str) -> PathBuf {
    plugin_grant_witness_path(global_root, name).with_extension("lock")
}

/// Delete everything in the namespace install directory except the tree the
/// `plugins` row now names.
///
/// Each upgrade used to leave `plugins/<ns>/<oldversion>/` behind. Those trees
/// are readable to every plugin backend — the install family is always
/// readable (§4.3) — and are enough to make a later `add` of that version
/// demand `--force`. One host row names one version, so nothing else under the
/// namespace directory is referenced: not an older version, not a `current`
/// link an earlier Orbit wrote beside them, not scratch a crashed install left.
///
/// Best effort, and only after the row is written: an install that landed is
/// not reported as a failure because a stale directory would not delete. The
/// caller holds [`lock_plugin_namespace`], so nothing here is another
/// operation's staging, replaced tree or newer version.
pub(super) fn prune_namespace(global_root: &Path, name: &str, keep: &Path) {
    let namespace_dir = plugin_namespace_dir(global_root, name);
    let Ok(entries) = std::fs::read_dir(&namespace_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Another install of this namespace may be building beside us.
        if path != keep && !is_live_plugin_build_dir(&entry.file_name()) {
            remove_install_scratch(&path);
        }
    }
}

pub(super) fn copy_tree(source: &Path, target: &Path) -> Result<(), OrbitError> {
    refuse_plugin_tree_symlinks(source)?;
    copy_tree_inner(source, source, target)
}

fn copy_tree_inner(tree_root: &Path, source: &Path, target: &Path) -> Result<(), OrbitError> {
    std::fs::create_dir_all(target)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", target.display())))?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", source.display())))?
    {
        let entry =
            entry.map_err(|error| OrbitError::Io(format!("read {}: {error}", source.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", path.display())))?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).ok();
            return Err(OrbitError::InvalidInput(plugin_symlink_refusal(
                path.strip_prefix(tree_root).unwrap_or(&path),
                target.as_deref(),
            )));
        }
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree_inner(tree_root, &path, &destination)?;
        } else {
            std::fs::copy(&path, &destination)
                .map_err(|error| OrbitError::Io(format!("copy {}: {error}", path.display())))?;
            copy_permissions(&path, &destination)?;
        }
    }
    Ok(())
}

fn copy_permissions(source: &Path, target: &Path) -> Result<(), OrbitError> {
    #[cfg(unix)]
    {
        let metadata = std::fs::metadata(source)
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", source.display())))?;
        std::fs::set_permissions(target, metadata.permissions())
            .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", target.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
    }
    Ok(())
}
