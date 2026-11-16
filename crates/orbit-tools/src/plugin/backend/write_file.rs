//! Strict admission for manifest-declared exact-file write capabilities.

use super::*;

/// One manifest exact-file grant: the path the manifest names, and the
/// physical file the sandbox grants for it. The two differ only when the
/// named file is an in-workspace symbolic link (`AGENTS.md` -> `CLAUDE.md`),
/// whose target is what the kernel checks a write against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestWriteFile {
    pub(crate) requested: PathBuf,
    pub(crate) granted: PathBuf,
    /// The workspace the grant was resolved under, so the spawn-time
    /// revalidation applies the same symlink rule.
    pub(crate) workspace_root: Option<PathBuf>,
}

/// Directory names inside a workspace that no exact-file grant may reach,
/// even through a symbolic link: repository and Orbit metadata.
const PROTECTED_WORKSPACE_NAMES: &[&str] = &[".git", ".orbit"];

/// Resolve one manifest exact-file grant to the physical file the sandbox may
/// write, inspecting it through no-follow directory descriptors. No file is
/// created, read, truncated or chmodded, and neither a symlink nor a missing
/// leaf turns a single-file request into a directory grant.
///
/// The file must be an existing regular file the operator owns, with one
/// link and no write access for others; group write is accepted, because a
/// checkout made under umask 0002 is group-writable and the grant gives the
/// plugin nothing the group does not already hold. Every ancestor is a real
/// directory owned by the operator or root; an operator-owned ancestor may be
/// group-writable, and any other writable ancestor must be sticky.
///
/// The leaf alone may be a symbolic link, and only when `workspace_root` is
/// given, the link sits inside it and its target resolves to a safe regular
/// file inside the same workspace, outside `.git` and `.orbit`. The target is
/// then what is granted; a link whose target leaves the workspace is refused.
pub(super) fn resolve(path: &Path, workspace_root: Option<&Path>) -> Result<PathBuf, OrbitError> {
    match inspect(path, workspace_root.is_some())? {
        Leaf::File => Ok(path.to_path_buf()),
        Leaf::Symlink => resolve_workspace_link(path, workspace_root),
    }
}

fn refusal(path: &Path, reason: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "spec.permissions.fs.write_files: `{}` {reason}; exact-file writes require an \
         existing user-owned regular file at a physical path, without hard links or write \
         access for others, reached without symbolic links except a final link inside the \
         workspace whose target stays inside it",
        path.display()
    ))
}

enum Leaf {
    File,
    Symlink,
}

fn resolve_workspace_link(
    path: &Path,
    workspace_root: Option<&Path>,
) -> Result<PathBuf, OrbitError> {
    let Some(workspace_root) = workspace_root else {
        return Err(refusal(path, "is a symbolic link outside a workspace"));
    };
    let workspace = workspace_root
        .canonicalize()
        .map_err(|_| refusal(path, "names a workspace that cannot be resolved"))?;
    // The ancestors were walked without following links, so the spelled
    // parent is already physical.
    if !path
        .parent()
        .is_some_and(|parent| parent.starts_with(&workspace))
    {
        return Err(refusal(path, "is a symbolic link outside the workspace"));
    }
    let target = path
        .canonicalize()
        .map_err(|_| refusal(path, "is a symbolic link whose target is missing"))?;
    let Ok(relative) = target.strip_prefix(&workspace) else {
        return Err(refusal(
            path,
            "is a symbolic link whose target leaves the workspace",
        ));
    };
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            PROTECTED_WORKSPACE_NAMES
                .iter()
                .any(|name| component.as_os_str() == *name)
        })
    {
        return Err(refusal(
            path,
            "is a symbolic link whose target is workspace metadata (`.git` or `.orbit`)",
        ));
    }
    // The target is canonical, so its walk meets no link; it must pass the
    // same file checks as a directly named file.
    match inspect(&target, false)? {
        Leaf::File => Ok(target),
        Leaf::Symlink => Err(refusal(path, "is a chain of symbolic links")),
    }
}

fn inspect(path: &Path, allow_leaf_link: bool) -> Result<Leaf, OrbitError> {
    let refusal = |reason: &str| refusal(path, reason);
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        use std::path::Component;

        // Keep the spelling the sandbox will compile. Lexically removing
        // `..` before walking could validate a different object from the
        // physical path the kernel later grants.
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(refusal("is not an absolute path"));
        }
        let names: Vec<_> = components.collect();
        if names.is_empty() {
            return Err(refusal("does not name a file"));
        }
        // The macOS compiler emits the granted path as a glob unless it has no
        // `*` or `?`, so `grant?` would also allow the sibling `grantX`. Refuse
        // such a component, in the file or any ancestor, before anything is
        // opened: the compiled policy then can only name this one file. [ORB-24211]
        if names.iter().any(|component| {
            matches!(component, Component::Normal(name)
                if name.as_bytes().iter().any(|byte| matches!(byte, b'*' | b'?')))
        }) {
            return Err(refusal(
                "has a path component containing `*` or `?`, which the macOS sandbox reads as a pattern matching sibling entries",
            ));
        }
        // SAFETY: geteuid reads the process identity and has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")
            .map_err(|_| refusal("has no accessible physical root"))?;
        for (index, component) in names.iter().enumerate() {
            let Component::Normal(name) = component else {
                return Err(refusal("has an unsafe path component"));
            };
            let last = index + 1 == names.len();
            let name = std::ffi::CString::new(name.as_bytes())
                .map_err(|_| refusal("has an invalid path component"))?;
            if last && allow_leaf_link {
                // SAFETY: an all-zero `stat` is a valid out-parameter.
                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                // SAFETY: the parent descriptor is live, name is one
                // NUL-terminated component, and stat is writable.
                let status = unsafe {
                    libc::fstatat(
                        file.as_raw_fd(),
                        name.as_ptr(),
                        &mut stat,
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                };
                if status == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
                    return Ok(Leaf::Symlink);
                }
            }
            let flags = libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if last {
                    libc::O_WRONLY
                } else {
                    libc::O_RDONLY | libc::O_DIRECTORY
                };
            // SAFETY: the parent descriptor is live, name is one NUL-terminated
            // component, and successful descriptors gain exactly one owner.
            let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(refusal(
                    "is missing, inaccessible, or passes through a symbolic link",
                ));
            }
            // SAFETY: successful openat returned a new owned descriptor.
            file = unsafe { std::fs::File::from_raw_fd(fd) };
            let metadata = file
                .metadata()
                .map_err(|_| refusal("cannot be inspected"))?;
            let sticky = metadata.mode() & 0o1000 != 0;
            if last {
                if !metadata.is_file()
                    || metadata.uid() != uid
                    || metadata.nlink() != 1
                    || metadata.mode() & 0o002 != 0
                {
                    return Err(refusal("is not a safe user-owned regular file"));
                }
            } else if !metadata.is_dir()
                || (metadata.uid() != uid && metadata.uid() != 0)
                || (metadata.mode() & 0o002 != 0 && !sticky)
                || (metadata.mode() & 0o020 != 0 && metadata.uid() != uid && !sticky)
            {
                return Err(refusal("passes through an unsafe directory"));
            }
        }
        Ok(Leaf::File)
    }
    #[cfg(not(unix))]
    {
        let _ = allow_leaf_link;
        Err(refusal("is unsupported on this platform"))
    }
}
