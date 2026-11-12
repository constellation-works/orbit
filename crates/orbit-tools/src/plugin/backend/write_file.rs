//! Strict admission for manifest-declared exact-file write capabilities.

use super::*;

/// Inspect an existing file through no-follow directory descriptors. No file
/// is created, read, truncated or chmodded. In particular, neither a symlink
/// nor a missing leaf turns a single-file request into a directory grant.
pub(super) fn validate(path: &Path) -> Result<(), OrbitError> {
    let refusal = |reason: &str| {
        OrbitError::PolicyDenied(format!(
            "spec.permissions.fs.write_files: `{}` {reason}; exact-file writes require an \
             existing user-owned regular file at a physical path, without symbolic links, \
             hard links, or group/other write access",
            path.display()
        ))
    };
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
            if last {
                if !metadata.is_file()
                    || metadata.uid() != uid
                    || metadata.nlink() != 1
                    || metadata.mode() & 0o022 != 0
                {
                    return Err(refusal("is not a safe user-owned regular file"));
                }
            } else if !metadata.is_dir()
                || (metadata.uid() != uid && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                return Err(refusal("passes through an unsafe directory"));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    Err(refusal("is unsupported on this platform"))
}
