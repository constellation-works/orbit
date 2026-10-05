use super::*;

pub(super) fn cwd_is_writable_root(cwd: &Path, writable_roots: &[PathBuf]) -> bool {
    writable_roots
        .iter()
        .any(|root| cwd.starts_with(root) || root.starts_with(cwd))
}

/// Bind the managed worktree (and its `target/` directory) at stable `/tmp`
/// paths inside this sandbox's private tmpfs. Toolchain wrappers can rewrite
/// absolute paths onto those mounts so compiler caches hit across worktrees
/// without sharing a mutable Cargo target directory. [ORB-11259]
pub(super) fn append_stable_toolchain_mounts(
    out: &mut Vec<String>,
    cwd: &Path,
) -> Result<(), OrbitError> {
    let target = cwd.join("target");
    std::fs::create_dir_all(&target).map_err(|error| {
        OrbitError::Execution(format!(
            "create managed-worktree target dir `{}` for stable build mount: {error}",
            target.display()
        ))
    })?;
    let target = canonical_existing(&target, "stable build mount")?;
    // Bind sources are host paths: a second bind does not inherit the first
    // destination's policy overlays. Replay the ordered mounts through both
    // aliases, clipping a containing restriction to the alias root itself.
    let policy_mounts: Vec<_> = out
        .windows(3)
        .filter(|args| matches!(args[0].as_str(), "--bind" | "--ro-bind"))
        .filter(|args| args[1] == args[2] && args[1] != "/")
        .map(|args| (args[0].clone(), PathBuf::from(&args[1])))
        .collect();
    out.extend([
        "--dir".to_string(),
        LINUX_STABLE_WORKSPACE_MOUNT.to_string(),
        "--bind".to_string(),
        cwd.display().to_string(),
        LINUX_STABLE_WORKSPACE_MOUNT.to_string(),
        "--dir".to_string(),
        LINUX_STABLE_BUILD_MOUNT.to_string(),
        "--bind".to_string(),
        target.display().to_string(),
        LINUX_STABLE_BUILD_MOUNT.to_string(),
    ]);
    for (root, alias) in [
        (cwd, Path::new(LINUX_STABLE_WORKSPACE_MOUNT)),
        (target.as_path(), Path::new(LINUX_STABLE_BUILD_MOUNT)),
    ] {
        for (option, source) in &policy_mounts {
            let (source, destination) = if let Ok(relative) = source.strip_prefix(root) {
                (source.as_path(), alias.join(relative))
            } else if root.starts_with(source) {
                (root, alias.to_path_buf())
            } else {
                continue;
            };
            out.extend([
                option.clone(),
                source.display().to_string(),
                destination.display().to_string(),
            ]);
        }
    }
    Ok(())
}

/// Subdirectories of `$CARGO_HOME` a sandboxed build must be able to write.
const CARGO_WRITABLE_CACHE_SUBDIRS: &[&str] = &["registry", "git"];

/// `$CARGO_HOME` lock files that serialize concurrent writers of those caches.
const CARGO_PACKAGE_CACHE_LOCK_FILES: &[&str] = &[".package-cache", ".package-cache-mutate"];

/// Whether the effective profile grants any write at all. A profile whose
/// `modify` rules are all negated confines the child to a read-only host, and
/// no convenience grant may quietly turn it into a writer.
pub(super) fn profile_grants_write(profile: &ResolvedFsProfile) -> bool {
    profile.modify.iter().any(|rule| !rule.starts_with('!'))
}

/// Resolve Cargo's home directory the way cargo itself does: `$CARGO_HOME`
/// when set, otherwise the documented `$HOME/.cargo` default.
pub(super) fn cargo_home_dir() -> Option<PathBuf> {
    fn non_empty(name: &str) -> Option<PathBuf> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    non_empty("CARGO_HOME").or_else(|| non_empty("HOME").map(|home| home.join(".cargo")))
}

/// Bind Cargo's shared download caches writable inside the namespace.
///
/// The backend's read-only bind of `/` makes every host path immutable, which
/// is right for the host but wrong for the one tree a build legitimately
/// populates: `cargo fetch` writes the downloaded `.crate` under
/// `$CARGO_HOME/registry/cache`, unpacks it under `registry/src`, refreshes the
/// index sidecar under `registry/index/<registry>/.cache`, and clones a git
/// dependency under `$CARGO_HOME/git`. Without these mounts a worker whose
/// lockfile names a single crate the host has not cached yet fails its build
/// with `failed to open .../registry/cache/<crate>.crate: Read-only file
/// system`, and stays silent until then, because a fully warm cache needs no
/// write at all. The macOS profile grants the same paths, so the two backends
/// answer the question the same way. [ORB-12469]
///
/// The two `.package-cache*` locks are bound for the same reason macOS grants
/// them: cargo treats a lock it cannot open as a read-only registry and
/// proceeds *unlocked*, so leaving them read-only while the registry is
/// writable would let concurrent workers mutate one shared registry with no
/// serialization.
///
/// Deliberately not bound: `$CARGO_HOME` itself, `$CARGO_HOME/bin`, and the
/// credential files beside them, all of which stay under the read-only bind.
/// The caller emits these mounts only for a profile that already grants some
/// write, so a reviewer or other read-only profile keeps a fully immutable
/// host — the same rule the global host cache root follows. [ORB-11259]
/// Bubblewrap cannot bind a source that does not exist, so an absent path is
/// skipped rather than created on the host; cargo then sees the same read-only
/// cache it saw before, which is the pre-existing behavior and not a new
/// failure mode.
pub(super) fn append_cargo_download_cache_mounts(out: &mut Vec<String>, cargo_home: Option<&Path>) {
    let Some(cargo_home) = cargo_home else {
        return;
    };
    for relative in CARGO_WRITABLE_CACHE_SUBDIRS
        .iter()
        .chain(CARGO_PACKAGE_CACHE_LOCK_FILES)
    {
        let path = cargo_home.join(relative);
        if path.exists() {
            push_mount(out, "--bind", &path);
        }
    }
}

pub(super) fn push_mount(args: &mut Vec<String>, option: &str, path: &Path) {
    let rendered = path.display().to_string();
    args.push(option.to_string());
    args.push(rendered.clone());
    args.push(rendered);
}
