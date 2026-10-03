use std::ffi::{CString, OsString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

use orbit_engine::DispatchError;

use super::provider_state::validated_linux_provider_state_root;

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

pub(super) fn open_runtime_file(root: &Path, file: &Path) -> Result<OwnedFd, DispatchError> {
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

fn mkdir_at(parent: &OwnedFd, name: &std::ffi::OsStr, path: &Path) -> Result<(), DispatchError> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| runtime_open_error(path, std::io::Error::from_raw_os_error(libc::EINVAL)))?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(runtime_open_error(path, std::io::Error::last_os_error()));
    }
    Ok(())
}

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
/// directory creation in
/// [`append_runtime_directory_grant`](super::runtime_grants::append_runtime_directory_grant)
/// and the writable
/// grant derived from it outside the root, so the path is resolved as far as it
/// already exists *before* any caller creates anything, and the result must
/// still live under the root.
///
/// `None` means the descendant escapes the root; the caller drops that grant
/// rather than following it.
pub(super) fn validated_linux_runtime_descendant(
    root: &Path,
    relative: &str,
) -> Result<Option<PathBuf>, DispatchError> {
    validated_linux_runtime_path(root, &root.join(relative))
}

pub(super) fn validated_linux_runtime_path(
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
pub(super) fn resolved_existing_ancestor(path: &Path) -> Result<Option<PathBuf>, DispatchError> {
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
