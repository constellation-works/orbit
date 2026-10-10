use std::fs::File;
use std::path::Path;

use orbit_engine::DispatchError;
use orbit_engine::LinuxRuntimeWriteAuthority;
use orbit_types::policy::ResolvedFsProfile;

use crate::OrbitRuntime;

use super::resolve::{append_unique_modify_root, codex_side_write_roots, side_root_store};
use super::runtime_paths::{
    open_or_create_runtime_directory, open_runtime_file, validated_linux_runtime_descendant,
    validated_linux_runtime_path, validated_linux_runtime_root,
};

pub(super) fn append_linux_runtime_write_roots(
    runtime: &OrbitRuntime,
    _subprocess_cwd: Option<&Path>,
    grants_workspace_modify: bool,
    resolved: &mut ResolvedFsProfile,
    authority: &mut Vec<LinuxRuntimeWriteAuthority>,
) -> Result<(), DispatchError> {
    let global = validated_linux_runtime_root(&runtime.paths().global_dir)?;
    let workspace = validated_linux_runtime_root(&runtime.paths().orbit_dir)?;

    for relative in ["state/logs", "state/audit", "tasks"] {
        append_runtime_directory_grant(&global, relative, resolved, authority)?;
    }
    append_runtime_sqlite_grants(&global, "orbit.db", resolved, authority)?;

    if !grants_workspace_modify {
        return Ok(());
    }

    for relative in [
        "tasks",
        "frictions",
        "state/audit",
        "state/logs",
        "state/job-runs",
    ] {
        append_runtime_directory_grant(&workspace, relative, resolved, authority)?;
    }
    append_runtime_sqlite_grants(&workspace, "state/semantic.db", resolved, authority)?;

    // Language-neutral host cache for toolchain artifacts shared across
    // worktrees (compiler caches, etc.). Implementer-only so read-only
    // profiles stay non-writers. Not a workspace `.orbit` path and not a
    // shared Cargo target directory. [ORB-11259]
    append_runtime_directory_grant(&global, "cache", resolved, authority)?;

    Ok(())
}

/// Grant Codex's `--add-dir` side roots as validated runtime store
/// directories, never as bare whole-tree binds of a runtime root. [ORB-14538]
pub(super) fn append_linux_codex_side_write_roots(
    runtime: &OrbitRuntime,
    provider: &str,
    resolved: &mut ResolvedFsProfile,
    authority: &mut Vec<LinuxRuntimeWriteAuthority>,
) -> Result<(), DispatchError> {
    let side_roots = codex_side_write_roots(runtime, provider)?;
    if side_roots.is_empty() {
        return Ok(());
    }
    let global = validated_linux_runtime_root(&runtime.paths().global_dir)?;
    let workspace = validated_linux_runtime_root(&runtime.paths().orbit_dir)?;
    for dir in side_roots {
        if let Some((root, relative)) = side_root_store(&[&global, &workspace], &dir) {
            append_runtime_directory_grant(root, &relative, resolved, authority)?;
        }
    }
    Ok(())
}

/// Grant one runtime store directory, creating it when it is missing.
///
/// A descendant that resolves outside its runtime root is skipped instead of
/// created: the grant only exists so nested Orbit processes can initialize
/// their own stores, so dropping it costs a convenience, while a hard failure
/// would break dispatch on every host that legitimately relocates a store
/// behind a symlink [ORB-11992].
// pub(super) widened for the sibling tests/ layout.
pub(super) fn append_runtime_directory_grant(
    root: &Path,
    relative: &str,
    resolved: &mut ResolvedFsProfile,
    authority: &mut Vec<LinuxRuntimeWriteAuthority>,
) -> Result<(), DispatchError> {
    let Some(directory) = validated_linux_runtime_descendant(root, relative)? else {
        tracing::warn!(
            runtime_root = %root.display(),
            store = relative,
            "skipping sandbox grant for a runtime store that resolves outside its runtime root"
        );
        return Ok(());
    };
    if authority.iter().any(|granted| granted.path == directory) {
        return Ok(());
    }

    let handle = open_or_create_runtime_directory(root, &directory)?;
    append_unique_modify_root(resolved, directory.display().to_string());
    authority.push(LinuxRuntimeWriteAuthority {
        path: directory,
        handle: std::sync::Arc::new(File::from(handle)),
        wal_file_set_lease: None,
    });
    Ok(())
}

/// Grant one SQLite sidecar of a runtime store, when it is already present.
///
/// Sidecars are never created here — SQLite writes them next to a database it
/// opens — so an absent one simply yields no grant. Only a regular file inside
/// the runtime root is granted: a sidecar that is a dangling or escaping
/// symlink would otherwise hand the sandbox a writable bind on whatever the
/// link names.
// pub(super) widened for the sibling tests/ layout.
#[cfg(test)]
pub(super) fn append_runtime_sidecar_grant(
    root: &Path,
    relative: &str,
    resolved: &mut ResolvedFsProfile,
    authority: &mut Vec<LinuxRuntimeWriteAuthority>,
) -> Result<(), DispatchError> {
    append_runtime_sidecar_grant_with_lease(root, &root.join(relative), resolved, authority, None)
}

/// Grant a runtime SQLite database and the sidecars belonging to its accepted
/// canonical path.
///
/// Containment must be decided before SQLite opens the lease: an authorized
/// in-root alias is resolved away before `SQLITE_OPEN_NOFOLLOW` reaches the
/// trust boundary, while an escaping alias is dropped without opening it.
// pub(super) widened for the sibling tests/ layout.
pub(super) fn append_runtime_sqlite_grants(
    root: &Path,
    relative: &str,
    resolved: &mut ResolvedFsProfile,
    authority: &mut Vec<LinuxRuntimeWriteAuthority>,
) -> Result<(), DispatchError> {
    let Some(database) = validated_linux_runtime_descendant(root, relative)? else {
        tracing::warn!(
            runtime_root = %root.display(),
            database = relative,
            "skipping sandbox grants for a database that resolves outside its runtime root"
        );
        return Ok(());
    };

    let lease = orbit_common::storage::sqlite::lease_wal_file_set(&database)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?
        .map(std::sync::Arc::new);

    for suffix in ["", "-wal", "-shm"] {
        let mut sidecar = database.as_os_str().to_os_string();
        sidecar.push(suffix);
        append_runtime_sidecar_grant_with_lease(
            root,
            Path::new(&sidecar),
            resolved,
            authority,
            lease.clone(),
        )?;
    }
    Ok(())
}

fn append_runtime_sidecar_grant_with_lease(
    root: &Path,
    candidate: &Path,
    resolved: &mut ResolvedFsProfile,
    authority: &mut Vec<LinuxRuntimeWriteAuthority>,
    wal_file_set_lease: Option<std::sync::Arc<orbit_common::storage::sqlite::WalFileSetLease>>,
) -> Result<(), DispatchError> {
    let Some(file) = validated_linux_runtime_path(root, candidate)? else {
        tracing::warn!(
            runtime_root = %root.display(),
            sidecar = %candidate.display(),
            "skipping sandbox grant for a database sidecar that resolves outside its runtime root"
        );
        return Ok(());
    };

    match std::fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.is_file() => {
            let handle = open_runtime_file(root, &file)?;
            append_unique_modify_root(resolved, file.display().to_string());
            authority.push(LinuxRuntimeWriteAuthority {
                path: file,
                handle: std::sync::Arc::new(File::from(handle)),
                wal_file_set_lease,
            });
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DispatchError::CliInvocationPermanent(format!(
            "inspect Linux sandbox runtime sidecar `{}`: {error}",
            file.display()
        ))),
    }
}
