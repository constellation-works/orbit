#[cfg(target_os = "linux")]
use std::ffi::{CString, OsString};
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use orbit_engine::LinuxRuntimeWriteAuthority;
use orbit_engine::RuntimeHost;
use orbit_engine::{DispatchError, ResolvedSandbox};
use orbit_types::policy::{ResolvedFsProfile, UNRESTRICTED_FS_PROFILE};
use orbit_types::workflow::ExecutorSandboxKind;

use crate::OrbitRuntime;

pub(crate) fn resolve_executor_sandbox(
    runtime: &OrbitRuntime,
    provider: &str,
    fs_profile: Option<&str>,
    subprocess_cwd: Option<&Path>,
) -> Result<Option<ResolvedSandbox>, DispatchError> {
    let executor = runtime.get_executor_def(provider).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "load executor `{provider}` for sandbox resolution: {err}"
        ))
    })?;
    let Some(executor) = executor else {
        return Ok(None);
    };
    let Some(kind) = executor.sandbox else {
        return Ok(None);
    };
    match kind {
        // Carry explicit off through preparation so the runner can suppress
        // provider-inner sandboxing and audit the choice without probing an OS
        // wrapper or resolving filesystem grants that will not be enforced.
        ExecutorSandboxKind::Off => Ok(Some(ResolvedSandbox {
            kind,
            fs_profile: ResolvedFsProfile {
                name: UNRESTRICTED_FS_PROFILE.to_string(),
                read: Vec::new(),
                modify: Vec::new(),
            },
            allow_fallback: false,
            managed_worktree: false,
            runtime_write_authority: Vec::new(),
        })),
        ExecutorSandboxKind::MacosSandboxExec => {
            #[cfg(not(target_os = "macos"))]
            {
                Err(DispatchError::CliInvocationFailed(format!(
                    "executor `{provider}` declares sandbox `macos-sandbox-exec` but current platform is `{}`",
                    std::env::consts::OS
                )))
            }
            #[cfg(target_os = "macos")]
            {
                // Read-only reviewer activities may run from an invocation-owned
                // inspection checkout, so their read grants must follow that
                // checkout. Implementer profiles stay anchored at the registered
                // workspace; the active worktree is re-allowed separately below
                // after the workspace's `.orbit` deny rules.
                let profile_root = if fs_profile == Some("reviewer") {
                    subprocess_cwd
                } else {
                    None
                };
                let mut resolved = resolve_fs_profile_absolute(runtime, fs_profile, profile_root)
                    .map_err(|err| {
                    DispatchError::CliInvocationFailed(format!(
                        "resolve fsProfile for sandbox: {err}"
                    ))
                })?;
                append_codex_side_write_roots(runtime, provider, &mut resolved)?;
                append_orbit_child_runtime_write_roots(runtime, &mut resolved);
                append_active_worktree_root(runtime, subprocess_cwd, &mut resolved);
                append_recovery_authority_deny(runtime, &mut resolved)?;
                Ok(Some(ResolvedSandbox {
                    kind,
                    fs_profile: resolved,
                    allow_fallback: executor.allow_fallback,
                    managed_worktree: false,
                    runtime_write_authority: Vec::new(),
                }))
            }
        }
        ExecutorSandboxKind::LinuxBwrap => {
            #[cfg(not(target_os = "linux"))]
            {
                Err(DispatchError::CliInvocationFailed(format!(
                    "executor `{provider}` declares sandbox `linux-bwrap` but current platform is `{}`",
                    std::env::consts::OS
                )))
            }
            #[cfg(target_os = "linux")]
            {
                let mut resolved = resolve_fs_profile_absolute(runtime, fs_profile, subprocess_cwd)
                    .map_err(|err| {
                        DispatchError::CliInvocationFailed(format!(
                            "resolve fsProfile for linux-bwrap: {err}"
                        ))
                    })?;
                // Runtime and provider conveniences must not turn an activity
                // profile with an empty modify surface into a workspace writer.
                // Besides violating that profile, workspace re-allows make a
                // direct Bubblewrap invocation unable to enforce global
                // non-subtree denies such as `**/.env` for paths created after
                // spawn. Provider state and global Orbit runtime roots remain
                // available below; neither overlaps workspace-relative denies.
                let grants_workspace_modify =
                    resolved.modify.iter().any(|rule| !rule.starts_with('!'));
                if grants_workspace_modify {
                    append_codex_side_write_roots(runtime, provider, &mut resolved)?;
                }
                let mut runtime_write_authority = Vec::new();
                append_linux_runtime_write_roots(
                    runtime,
                    subprocess_cwd,
                    grants_workspace_modify,
                    &mut resolved,
                    &mut runtime_write_authority,
                )?;
                append_linux_provider_state_roots(provider, &mut resolved)?;
                append_recovery_authority_deny(runtime, &mut resolved)?;
                // Host Git state is never a provider convenience grant. Append
                // these last so even a side root inside metadata stays denied.
                crate::runtime::git_sandbox::append_linux_git_denies(
                    &runtime.paths().repo_root,
                    &mut resolved,
                )
                .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
                if let Some(cwd) = subprocess_cwd {
                    crate::runtime::git_sandbox::append_linux_git_denies(cwd, &mut resolved)
                        .map_err(|error| {
                            DispatchError::CliInvocationPermanent(error.to_string())
                        })?;
                }
                let managed_worktree = subprocess_cwd
                    .and_then(|cwd| active_worktree_subpath(runtime, cwd))
                    .is_some();
                Ok(Some(ResolvedSandbox {
                    kind,
                    fs_profile: resolved,
                    allow_fallback: executor.allow_fallback,
                    managed_worktree,
                    runtime_write_authority,
                }))
            }
        }
    }
}

/// Resolve the activity's fsProfile against the active policy, then expand
/// every workspace-relative `read` / `modify` rule to an absolute path under
/// the workspace root. The kernel's `subpath` predicate is meaningless for
/// relative paths, so this is the layer that turns Orbit's policy into a
/// payload `sandbox-exec` can enforce.
fn resolve_fs_profile_absolute(
    runtime: &OrbitRuntime,
    fs_profile: Option<&str>,
    workspace_override: Option<&Path>,
) -> Result<ResolvedFsProfile, orbit_common::OrbitError> {
    let profile_name = fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE);
    let resolved = runtime
        .policy_engine()
        .def()
        .effective_profile(profile_name)?;
    let workspace_root = workspace_override
        .unwrap_or(&runtime.paths().repo_root)
        .canonicalize()
        .unwrap_or_else(|_| {
            workspace_override.map_or_else(
                || runtime.paths().repo_root.clone(),
                std::path::Path::to_path_buf,
            )
        });
    let workspace_str = workspace_root.display().to_string();

    Ok(ResolvedFsProfile {
        name: resolved.name,
        read: resolved
            .read
            .into_iter()
            .map(|rule| absolutize_rule(&workspace_str, &rule))
            .collect(),
        modify: resolved
            .modify
            .into_iter()
            .map(|rule| absolutize_rule(&workspace_str, &rule))
            .collect(),
    })
}

fn append_codex_side_write_roots(
    runtime: &OrbitRuntime,
    provider: &str,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), DispatchError> {
    // Codex is the only `backend: cli` provider that ships its own writable
    // root surface (`--add-dir` fed from `writable_dirs_json`). Claude and
    // Gemini have no analogous CLI flag — their startup-time writes are
    // confined to their state directories, which `compile_macos_sandbox_profile`
    // already grants via the per-provider state-dir allowances. If a future
    // provider gains a side-root surface, add a sibling appender. See
    // T20260428-14.
    if provider != "codex" {
        return Ok(());
    }

    let config = RuntimeHost::agent_provider_config(runtime);
    let Some(raw_dirs) = config.get("writable_dirs_json") else {
        return Ok(());
    };
    let writable_dirs: Vec<String> = serde_json::from_str(raw_dirs).map_err(|err| {
        DispatchError::CliInvocationFailed(format!(
            "parse codex writable_dirs_json for sandbox: {err}"
        ))
    })?;
    if writable_dirs.is_empty() {
        return Ok(());
    }

    let workspace_root = runtime
        .paths()
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().repo_root.clone());
    let workspace_str = workspace_root.display().to_string();
    for dir in writable_dirs {
        let Some(root) = absolutize_side_write_root(&workspace_str, &dir) else {
            continue;
        };
        // Append even when the root already appears earlier: SBPL is
        // last-match-wins, and these host-owned roots must land after
        // policy-derived denies such as `.orbit/**`.
        resolved.modify.push(root);
    }
    Ok(())
}

/// Allow the nested Orbit processes launched by provider CLIs to initialize
/// only the runtime stores they need while staying inside the outer sandbox.
///
/// Gemini, Antigravity, and Claude do not have a codex-style `--add-dir` side
/// channel, but their MCP/tool calls still execute `orbit ...` as a
/// sandbox-inherited child.
/// Those child processes initialize global logs/audit/databases/tasks plus the
/// workspace stores exposed by activity tool allowlists.
///
/// Inventory boundary: this list follows currently activity-exposed Orbit write
/// tools. Registered-but-not-exposed stores such as ADRs and graph write roots
/// stay denied until the corresponding tools are added to those activity
/// allowlists. Keep the grants path-shaped instead of re-allowing the whole
/// home directory or workspace `.orbit` tree.
#[cfg(target_os = "macos")]
fn append_orbit_child_runtime_write_roots(
    runtime: &OrbitRuntime,
    resolved: &mut ResolvedFsProfile,
) {
    let global_root = runtime
        .paths()
        .global_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().global_dir.clone());
    let global = global_root.display().to_string();

    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let workspace = workspace_orbit.display().to_string();

    for root in [
        format!("{global}/state/logs/**"),
        format!("{global}/state/audit/**"),
        format!("{global}/orbit.db*"),
        format!("{global}/tasks/**"),
        // Language-neutral host cache seam shared across worktrees. Not an
        // activity-tool store and not a shared Cargo target directory.
        // [ORB-11259]
        format!("{global}/cache/**"),
        format!("{workspace}/tasks/**"),
        format!("{workspace}/frictions/**"),
        format!("{workspace}/state/audit/**"),
        format!("{workspace}/state/logs/**"),
        format!("{workspace}/state/semantic.db*"),
    ] {
        append_unique_modify_root(resolved, root);
    }
}

/// Deny the host-only recovery authority store, after every convenience grant.
///
/// No grant above names this root, so the rule is a tripwire that keeps a
/// future broadening of the `<global>` grants from reopening it. Both sandbox
/// kinds get it: the boundary is a property of the store, not of one OS.
fn append_recovery_authority_deny(
    runtime: &OrbitRuntime,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), DispatchError> {
    crate::runtime::recovery_authority::append_recovery_authority_denies(
        &runtime.paths().global_dir,
        resolved,
    )
    .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))
}

fn append_unique_modify_root(resolved: &mut ResolvedFsProfile, root: String) {
    if !resolved.modify.iter().any(|entry| entry == &root) {
        resolved.modify.push(root);
    }
}

#[cfg(target_os = "linux")]
fn append_linux_runtime_write_roots(
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

/// Grant one runtime store directory, creating it when it is missing.
///
/// A descendant that resolves outside its runtime root is skipped instead of
/// created: the grant only exists so nested Orbit processes can initialize
/// their own stores, so dropping it costs a convenience, while a hard failure
/// would break dispatch on every host that legitimately relocates a store
/// behind a symlink [ORB-11992].
// pub(super) widened for the sibling tests/ layout.
#[cfg(target_os = "linux")]
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
#[cfg(all(target_os = "linux", test))]
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
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
pub(super) fn open_or_create_runtime_directory(
    root: &Path,
    directory: &Path,
) -> Result<OwnedFd, DispatchError> {
    let relative = directory.strip_prefix(root).map_err(|_| {
        DispatchError::CliInvocationPermanent(format!(
            "Linux sandbox runtime store `{}` escaped `{}`",
            directory.display(),
            root.display()
        ))
    })?;
    let mut current =
        open_directory_at(None, root).map_err(|error| runtime_open_error(root, error))?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "Linux sandbox runtime store `{}` contains an invalid component",
                directory.display()
            )));
        };
        match open_directory_at(Some(&current), Path::new(name)) {
            Ok(next) => current = next,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                mkdir_at(&current, name, directory)?;
                current = open_directory_at(Some(&current), Path::new(name))
                    .map_err(|error| runtime_open_error(directory, error))?;
            }
            Err(error) => return Err(runtime_open_error(directory, error)),
        }
    }
    Ok(current)
}

#[cfg(target_os = "linux")]
fn open_runtime_file(root: &Path, file: &Path) -> Result<OwnedFd, DispatchError> {
    let relative = file
        .strip_prefix(root)
        .map_err(|_| runtime_open_error(file, std::io::Error::from_raw_os_error(libc::EXDEV)))?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent_path = root.join(parent);
    let directory = open_or_create_runtime_directory(root, &parent_path)?;
    let name = relative
        .file_name()
        .ok_or_else(|| runtime_open_error(file, std::io::Error::from_raw_os_error(libc::EINVAL)))?;
    let name = CString::new(name.as_bytes())
        .map_err(|_| runtime_open_error(file, std::io::Error::from_raw_os_error(libc::EINVAL)))?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let raw = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if raw < 0 {
        return Err(runtime_open_error(file, std::io::Error::last_os_error()));
    }
    let opened = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    let metadata = opened
        .metadata()
        .map_err(|error| runtime_open_error(file, error))?;
    if !metadata.is_file() {
        return Err(runtime_open_error(
            file,
            std::io::Error::from_raw_os_error(libc::EINVAL),
        ));
    }
    Ok(opened.into())
}

#[cfg(target_os = "linux")]
fn open_directory_at(parent: Option<&OwnedFd>, path: &Path) -> std::io::Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let raw = match parent {
        Some(parent) => unsafe { libc::openat(parent.as_raw_fd(), path.as_ptr(), flags) },
        None => unsafe { libc::open(path.as_ptr(), flags) },
    };
    if raw < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

#[cfg(target_os = "linux")]
fn mkdir_at(parent: &OwnedFd, name: &std::ffi::OsStr, path: &Path) -> Result<(), DispatchError> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| runtime_open_error(path, std::io::Error::from_raw_os_error(libc::EINVAL)))?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) };
    if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(runtime_open_error(path, std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn runtime_open_error(path: &Path, error: std::io::Error) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!(
        "open Linux sandbox runtime object `{}` without following links: {error}",
        path.display()
    ))
}

/// Resolve a store Orbit owns beneath an already-validated runtime root.
///
/// The root is canonical, but nothing below it is. An intermediate or leaf
/// symlink under the root — or a `..` in `relative` — would move both the
/// directory creation in [`append_runtime_directory_grant`] and the writable
/// grant derived from it outside the root, so the path is resolved as far as it
/// already exists *before* any caller creates anything, and the result must
/// still live under the root.
///
/// `None` means the descendant escapes the root; the caller drops that grant
/// rather than following it.
#[cfg(target_os = "linux")]
pub(super) fn validated_linux_runtime_descendant(
    root: &Path,
    relative: &str,
) -> Result<Option<PathBuf>, DispatchError> {
    validated_linux_runtime_path(root, &root.join(relative))
}

#[cfg(target_os = "linux")]
fn validated_linux_runtime_path(
    root: &Path,
    candidate: &Path,
) -> Result<Option<PathBuf>, DispatchError> {
    let Some(resolved) = resolved_existing_ancestor(candidate)? else {
        return Ok(None);
    };
    Ok(resolved.starts_with(root).then_some(resolved))
}

/// Split a path into the deepest ancestor that already exists and the
/// components that do not, canonicalize that ancestor, and rejoin them.
///
/// Canonicalizing resolves every symlink on the existing part, which is what
/// makes the result usable as a containment decision: whatever the caller does
/// next happens at the real location, not at the name it was given. `Ok(None)`
/// means the walk ran out of ancestors; callers phrase their own rejection.
#[cfg(target_os = "linux")]
fn resolved_existing_ancestor(path: &Path) -> Result<Option<PathBuf>, DispatchError> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::<OsString>::new();

    let canonical_existing = loop {
        match existing.canonicalize() {
            Ok(canonical) => break canonical,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Ok(None);
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Ok(None);
                }
            }
            Err(error) => {
                return Err(DispatchError::CliInvocationPermanent(format!(
                    "inspect Linux sandbox path ancestor `{}`: {error}",
                    existing.display()
                )));
            }
        }
    };

    let mut resolved = canonical_existing;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(Some(resolved))
}

/// Validate runtime roots before constructing any sandbox path beneath them.
///
/// These roots can be selected through the managed-run registry locator or an
/// explicit root override. Unlike a provider state root, a runtime root is
/// never created here: it must already exist as a directory when a runtime is
/// resolving its executor sandbox, so the root is canonicalized first and the
/// directory check is made against the resolved location rather than the name
/// that was supplied. A root reached through a symlinked ancestor stays
/// supported and resolves to its real directory [ORB-11984].
///
/// The returned root bounds nothing on its own; every path built beneath it
/// goes through [`validated_linux_runtime_descendant`].
#[cfg(target_os = "linux")]
pub(super) fn validated_linux_runtime_root(path: &Path) -> Result<PathBuf, DispatchError> {
    let validated = validated_linux_provider_state_root(path, None)?;
    let canonical = validated.canonicalize().map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "canonicalize Linux sandbox runtime root `{}`: {error}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux sandbox runtime root `{}` must be an existing directory",
            path.display()
        )));
    }
    Ok(canonical)
}

#[cfg(target_os = "linux")]
fn append_linux_provider_state_roots(
    provider: &str,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), DispatchError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut directories = Vec::new();
    if let Some(path) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        directories.push(path);
    } else if let Some(home) = &home {
        directories.push(home.join(".codex"));
    }
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
        directories.push(path);
    } else if let Some(home) = &home {
        directories.push(home.join(".claude"));
    }
    if let Some(home) = &home {
        directories.push(home.join(".gemini"));
        directories.push(home.join(".grok"));
    }
    // [ORB-10946] Copilot's roots are appended only when Copilot is the
    // provider being dispatched. This mirrors the macOS gate, and it matters
    // more here than on macOS: every entry in this list is *created* by the
    // validated provider-root path below, so an unconditional entry would
    // mkdir a `~/.copilot` on hosts that have never installed the CLI.
    directories.extend(linux_copilot_state_roots(provider, home.as_deref()));
    directories.extend(linux_cursor_state_roots_with(provider, home.as_deref()));
    directories.extend(linux_pi_state_roots(provider, home.as_deref()));
    directories.extend(linux_opencode_state_roots(provider, home.as_deref()));
    for directory in directories {
        let canonical = ensure_linux_provider_directory(&directory, home.as_deref())?;
        append_unique_modify_root(resolved, canonical.display().to_string());
    }
    Ok(())
}

/// Reject a provider state root that would grant more than a provider directory.
///
/// `candidate` is the path being judged; `configured` is what the operator
/// supplied. They differ once symlinks have been resolved, and naming both keeps
/// a rejection traceable back to the setting that caused it.
#[cfg(target_os = "linux")]
fn reject_overbroad_linux_provider_state_root(
    candidate: &Path,
    configured: &Path,
    home: Option<&Path>,
) -> Result<(), DispatchError> {
    let describe = || {
        if candidate == configured {
            format!("`{}`", configured.display())
        } else {
            format!(
                "`{}` (resolved to `{}`)",
                configured.display(),
                candidate.display()
            )
        }
    };

    if candidate.parent().is_none() {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root {} must not be the filesystem root",
            describe()
        )));
    }

    if let Some(home) = home {
        let canonical_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        if canonical_home.starts_with(candidate) {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "Linux provider state root {} is broader than the user's home directory",
                describe()
            )));
        }
    }

    Ok(())
}

/// Resolve a provider state root before creating it on behalf of a child.
///
/// Provider-specific environment variables are operator-configurable, but
/// they must not turn sandbox preparation into an arbitrary path creator.
/// Reject relative paths, path traversal, and root/home-wide targets.
///
/// Symlinks are resolved rather than rejected. Hosts routinely reach `$HOME`
/// through a symlinked ancestor (OSTree systems ship `/home -> /var/home`), and
/// dotfile managers routinely make a provider directory itself a symlink; both
/// are ordinary configurations, not attacks. [ORB-11984]
///
/// Following symlinks means the configured path no longer bounds the grant, so
/// containment is enforced twice: once on what the operator supplied, and again
/// on the resolved destination. The returned path has an existing canonical
/// ancestor and only the validated missing suffix, so the caller can safely
/// materialize it.
#[cfg(target_os = "linux")]
pub(super) fn validated_linux_provider_state_root(
    path: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, DispatchError> {
    if !path.is_absolute() {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root `{}` must be absolute",
            path.display()
        )));
    }

    for component in path.components() {
        if !matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        ) {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "Linux provider state root `{}` must not contain traversal components",
                path.display()
            )));
        }
    }
    reject_overbroad_linux_provider_state_root(path, path, home)?;

    // Resolve the deepest part of the path that already exists, so a provider
    // directory that is itself a symlink to an existing directory validates
    // against its real destination.
    let Some(validated) = resolved_existing_ancestor(path)? else {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "Linux provider state root `{}` has no existing ancestor",
            path.display()
        )));
    };

    reject_overbroad_linux_provider_state_root(&validated, path, home)?;

    Ok(validated)
}

#[cfg(target_os = "linux")]
pub(super) fn ensure_linux_provider_directory(
    path: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, DispatchError> {
    let validated = validated_linux_provider_state_root(path, home)?;
    std::fs::create_dir_all(&validated).map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "create Linux provider state root `{}`: {error}",
            validated.display()
        ))
    })?;
    validated.canonicalize().map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "canonicalize Linux provider state root `{}`: {error}",
            validated.display()
        ))
    })
}

/// Writable state root for an active Cursor executor on Linux. The CLI stores
/// logged-in authentication, settings, permissions, and sessions under
/// `$HOME/.cursor`; no other provider receives this grant. [ORB-10945]
#[cfg(target_os = "linux")]
pub(super) fn linux_cursor_state_roots_with(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Cursor)
    {
        return Vec::new();
    }
    home.map(|home| vec![home.join(".cursor")])
        .unwrap_or_default()
}

/// Process-env wrapper around [`linux_pi_state_roots_with`].
#[cfg(target_os = "linux")]
fn linux_pi_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    linux_pi_state_roots_with(
        provider,
        home,
        std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .as_deref(),
    )
}

/// Writable state root for an active Pi executor on Linux. The CLI stores
/// `/login` credentials, settings, saved project trust decisions, installed
/// packages, and sessions under `$PI_CODING_AGENT_DIR` when set, otherwise
/// `$HOME/.pi`. No other provider receives this grant — every entry in the
/// caller's list is *created* by `ensure_linux_provider_directory`, so an
/// unconditional entry would mkdir a `~/.pi` on hosts that never installed Pi.
/// [ORB-11296]
#[cfg(target_os = "linux")]
pub(super) fn linux_pi_state_roots_with(
    provider: &str,
    home: Option<&Path>,
    pi_coding_agent_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Pi)
    {
        return Vec::new();
    }
    match pi_coding_agent_dir {
        Some(path) => vec![path.to_path_buf()],
        None => home.map(|home| vec![home.join(".pi")]).unwrap_or_default(),
    }
}

/// Process-env wrapper around [`linux_opencode_state_roots_with`].
#[cfg(target_os = "linux")]
fn linux_opencode_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let env_path = |name: &str| std::env::var_os(name).map(PathBuf::from);
    linux_opencode_state_roots_with(
        provider,
        home,
        OpencodeStateEnv {
            xdg_data_home: env_path("XDG_DATA_HOME"),
            xdg_config_home: env_path("XDG_CONFIG_HOME"),
            xdg_state_home: env_path("XDG_STATE_HOME"),
            xdg_cache_home: env_path("XDG_CACHE_HOME"),
            opencode_config_dir: env_path("OPENCODE_CONFIG_DIR"),
        },
    )
}

/// XDG roots that locate OpenCode's writable state on Linux.
#[cfg(target_os = "linux")]
#[derive(Default, Clone)]
pub(super) struct OpencodeStateEnv {
    pub(super) xdg_data_home: Option<PathBuf>,
    pub(super) xdg_config_home: Option<PathBuf>,
    pub(super) xdg_state_home: Option<PathBuf>,
    pub(super) xdg_cache_home: Option<PathBuf>,
    pub(super) opencode_config_dir: Option<PathBuf>,
}

/// Writable state roots for an active OpenCode executor on Linux.
///
/// OpenCode resolves every root through `xdg-basedir` and creates its data,
/// config, and state directories at startup, before it reads Orbit's envelope.
/// The data root holds `auth.json` from `opencode auth login`, the session and
/// message stores, and logs. No other provider receives this grant — every
/// entry in the caller's list is *created* by `ensure_linux_provider_directory`,
/// so an unconditional entry would mkdir an `~/.local/share/opencode` on hosts
/// that never installed OpenCode. [ORB-11295]
#[cfg(target_os = "linux")]
pub(super) fn linux_opencode_state_roots_with(
    provider: &str,
    home: Option<&Path>,
    env: OpencodeStateEnv,
) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Opencode)
    {
        return Vec::new();
    }
    let scoped = |xdg_base: Option<PathBuf>, home_relative_default: &[&str]| -> Option<PathBuf> {
        let base = xdg_base.or_else(|| {
            home.map(|home| {
                home_relative_default
                    .iter()
                    .fold(home.to_path_buf(), |path, segment| path.join(segment))
            })
        })?;
        Some(base.join("opencode"))
    };
    [
        scoped(env.xdg_data_home, &[".local", "share"]),
        env.opencode_config_dir
            .or_else(|| scoped(env.xdg_config_home, &[".config"])),
        scoped(env.xdg_state_home, &[".local", "state"]),
        scoped(env.xdg_cache_home, &[".cache"]),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Process-env wrapper around [`linux_copilot_state_roots_with`].
#[cfg(target_os = "linux")]
fn linux_copilot_state_roots(provider: &str, home: Option<&Path>) -> Vec<PathBuf> {
    linux_copilot_state_roots_with(
        provider,
        home,
        std::env::var_os("COPILOT_HOME")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .as_deref(),
    )
}

/// Writable roots an active Copilot executor needs on Linux: its
/// configuration/state directory (`$COPILOT_HOME`, else `$HOME/.copilot`) and
/// the launcher's bundled-package extraction cache (`$XDG_CACHE_HOME/copilot`,
/// else `$HOME/.cache/copilot`). Empty for every other provider. [ORB-10946]
///
/// The override values are parameters rather than direct env reads so the
/// gate can be asserted without mutating process state from a test.
// pub(super) widened for the sibling tests/ layout.
#[cfg(target_os = "linux")]
pub(super) fn linux_copilot_state_roots_with(
    provider: &str,
    home: Option<&Path>,
    copilot_home: Option<&Path>,
    xdg_cache_home: Option<&Path>,
) -> Vec<PathBuf> {
    if orbit_types::workflow::Provider::parse(provider).ok()
        != Some(orbit_types::workflow::Provider::Copilot)
    {
        return Vec::new();
    }
    let mut roots = Vec::with_capacity(2);
    match copilot_home {
        Some(path) => roots.push(path.to_path_buf()),
        None => {
            if let Some(home) = home {
                roots.push(home.join(".copilot"));
            }
        }
    }
    match xdg_cache_home {
        Some(path) => roots.push(path.join("copilot")),
        None => {
            if let Some(home) = home {
                roots.push(home.join(".cache").join("copilot"));
            }
        }
    }
    roots
}

/// Re-allow the active job-run worktree under `<workspace>/.orbit/state/worktrees/`
/// for every provider, after the policy's `denyModify .orbit/**` rule. Without
/// this, `task_pr_pipeline` runs whose subprocess cwd lives under
/// `.orbit/state/worktrees/orbit-jrun-…` cannot edit their own checkout under
/// the macOS sandbox: SBPL is last-match-wins, the broad `unrestricted` profile
/// allows `<workspace>/**` first, the global deny appends `!<workspace>/.orbit/**`
/// last, and codex was the only provider that re-asserted a writable side-root
/// after that. See T20260508-17.
///
/// Scope is deliberately narrow: only the calling subprocess's cwd is
/// re-allowed, and only when it canonicalizes to a direct child of
/// `<workspace>/.orbit/state/worktrees/`. Cwds outside that prefix yield no
/// change — we do not blanket-reallow `.orbit/**` for non-codex providers.
#[cfg(target_os = "macos")]
fn append_active_worktree_root(
    runtime: &OrbitRuntime,
    subprocess_cwd: Option<&Path>,
    resolved: &mut ResolvedFsProfile,
) {
    let Some(cwd) = subprocess_cwd else {
        return;
    };
    let Some(worktree_root) = active_worktree_subpath(runtime, cwd) else {
        return;
    };
    // Append after the policy denies; SBPL last-match-wins re-grants writes
    // inside the active worktree without widening any path outside it.
    resolved.modify.push(worktree_root);
}

fn active_worktree_subpath(runtime: &OrbitRuntime, subprocess_cwd: &Path) -> Option<String> {
    active_worktree_root(runtime, subprocess_cwd).map(|worktree| worktree.display().to_string())
}

fn active_worktree_root(runtime: &OrbitRuntime, subprocess_cwd: &Path) -> Option<PathBuf> {
    let cwd = subprocess_cwd
        .canonicalize()
        .unwrap_or_else(|_| subprocess_cwd.to_path_buf());
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let worktrees_root = workspace_orbit.join("state").join("worktrees");
    // Require the cwd to live strictly under `…/.orbit/state/worktrees/`.
    // A bare `worktrees` cwd would re-allow the entire registry; one path
    // segment deeper restricts the grant to a single jrun subtree.
    let relative = cwd.strip_prefix(&worktrees_root).ok()?;
    let mut components = relative.components();
    let first = components.next()?;
    Some(worktrees_root.join(first.as_os_str()))
}

fn absolutize_side_write_root(workspace_root: &str, path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let absolute = if PathBuf::from(trimmed).is_absolute() {
        PathBuf::from(trimmed)
    } else {
        let trimmed = trimmed.trim_start_matches("./");
        if trimmed.is_empty() || trimmed == "." {
            PathBuf::from(workspace_root)
        } else {
            PathBuf::from(workspace_root).join(trimmed)
        }
    };
    let normalized = absolute.canonicalize().unwrap_or(absolute);
    Some(normalized.display().to_string())
}

fn absolutize_rule(workspace_root: &str, rule: &str) -> String {
    let (negated, body) = rule
        .strip_prefix('!')
        .map(|rest| (true, rest))
        .unwrap_or((false, rule));
    let trimmed = body.trim_start_matches("./");
    let absolute = if PathBuf::from(trimmed).is_absolute() {
        trimmed.to_string()
    } else if trimmed.is_empty() || trimmed == "." {
        workspace_root.to_string()
    } else {
        format!("{}/{}", workspace_root.trim_end_matches('/'), trimmed)
    };
    if negated {
        format!("!{absolute}")
    } else {
        absolute
    }
}
