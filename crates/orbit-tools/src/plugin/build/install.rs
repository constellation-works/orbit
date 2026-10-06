//! Install declared build outputs and compute their artifact digest.

use std::io::Read;
use std::path::{Component, Path};

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginBuildOutput, PluginBuildOutputRecord, artifact_digest_preimage};
use sha2::{Digest, Sha256};

use super::super::source::MAX_UNPACKED_BYTES;

/// Copy each declared output from the build directory into `staging` (the
/// install's copy of the pristine plugin root) and record it (§3.4, §3.6).
///
/// `from` must be a physical regular file inside the build directory with
/// one link and no link anywhere on its path; `to` must not name a file the
/// pristine root holds. The installed mode keeps only the owner-execute bit
/// as `0755` or `0644`, so the digest does not depend on a host's umask.
/// The hash is taken from the staged copy, the bytes that are installed.
pub fn install_plugin_build_outputs(
    build_dir: &Path,
    outputs: &[PluginBuildOutput],
    staging: &Path,
) -> Result<Vec<PluginBuildOutputRecord>, OrbitError> {
    let mut total: u64 = 0;
    let mut records = Vec::new();
    for output in outputs {
        let from = physical_output(build_dir, &output.from)?;
        let to = staging.join(&output.to);
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(OrbitError::PolicyDenied(format!(
                "build output '{}' would replace a file the reviewed plugin tree ships; nothing \
                 was installed",
                output.to
            )));
        }
        let metadata = from.metadata().map_err(|error| {
            OrbitError::Io(format!("stat build output '{}': {error}", output.from))
        })?;
        let remaining = MAX_UNPACKED_BYTES.saturating_sub(total);
        if metadata.len() > remaining {
            return Err(OrbitError::PolicyDenied(format!(
                "the build outputs exceed {} MiB together; nothing was installed",
                MAX_UNPACKED_BYTES / (1024 * 1024)
            )));
        }
        if let Some(parent) = to.parent() {
            refuse_linked_parent(staging, parent)?;
            std::fs::create_dir_all(parent)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
        }
        let mut destination = std::fs::File::create_new(&to)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", to.display())))?;
        // Copy from the already-open descriptor, never by re-opening the
        // inspected path. Bound the actual bytes too: a surviving offline
        // descendant on macOS can still grow an open output file.
        let copied =
            std::io::copy(&mut from.take(remaining + 1), &mut destination).map_err(|error| {
                OrbitError::Io(format!("copy build output '{}': {error}", output.from))
            })?;
        if copied > remaining {
            return Err(OrbitError::PolicyDenied(format!(
                "the build outputs exceed {} MiB together; nothing was installed",
                MAX_UNPACKED_BYTES / (1024 * 1024)
            )));
        }
        total += copied;
        let mode = installed_mode(&metadata);
        set_mode(&to, mode)?;
        records.push(PluginBuildOutputRecord {
            to: output.to.clone(),
            mode,
            sha256: file_sha256(&to)?,
        });
    }
    Ok(records)
}

/// Walk the output through directory descriptors. Each open refuses links,
/// and an ancestor renamed after it was opened cannot redirect the next
/// component. The leaf is nonblocking so a swapped FIFO cannot hang Orbit.
fn physical_output(build_dir: &Path, relative: &str) -> Result<std::fs::File, OrbitError> {
    let refusal = |reason: &str| {
        OrbitError::PolicyDenied(format!(
            "build output '{relative}' {reason}; nothing was installed"
        ))
    };
    let components: Vec<Component<'_>> = Path::new(relative).components().collect();
    if components.is_empty() {
        return Err(refusal("is not a plain relative path"));
    }
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(build_dir)
            .map_err(|_| refusal("has no physical build directory"))?
    };
    #[cfg(not(unix))]
    let mut path = build_dir.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(refusal("is not a plain relative path"));
        };
        let last = index + 1 == components.len();
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(name.as_bytes())
                .map_err(|_| refusal("is not a plain relative path"))?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if last { 0 } else { libc::O_DIRECTORY };
            // SAFETY: `file` owns the live directory descriptor; `name` is
            // a NUL-terminated single component. A successful fd is owned
            // exactly once by the returned File.
            let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                return Err(refusal(match error.raw_os_error() {
                    Some(libc::ELOOP | libc::ENOTDIR) => {
                        "is or passes through a symbolic link or non-directory"
                    }
                    _ => "was not produced by the build",
                }));
            }
            // SAFETY: the successful openat returned an owned descriptor.
            file = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        #[cfg(unix)]
        let metadata = file
            .metadata()
            .map_err(|_| refusal("cannot be inspected"))?;
        #[cfg(not(unix))]
        let metadata = {
            path.push(name);
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| refusal("was not produced by the build"))?;
            if metadata.file_type().is_symlink() {
                return Err(refusal("is or passes through a symbolic link"));
            }
            metadata
        };
        if !last && !metadata.is_dir() {
            return Err(refusal("passes through something that is not a directory"));
        }
        if last {
            if !metadata.is_file() {
                return Err(refusal("is not a regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(refusal("is a hard link"));
                }
            }
        }
    }
    #[cfg(unix)]
    return Ok(file);
    #[cfg(not(unix))]
    std::fs::File::open(path).map_err(|_| refusal("cannot be opened"))
}

fn refuse_linked_parent(staging: &Path, parent: &Path) -> Result<(), OrbitError> {
    let Ok(relative) = parent.strip_prefix(staging) else {
        return Err(OrbitError::PolicyDenied(
            "a build output must stay inside the plugin root".to_string(),
        ));
    };
    let mut path = staging.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "build output directory {} is not a directory in the plugin tree",
                    path.display()
                )));
            }
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn installed_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o100 != 0 {
        0o755
    } else {
        0o644
    }
}

#[cfg(not(unix))]
fn installed_mode(_metadata: &std::fs::Metadata) -> u32 {
    0o644
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), OrbitError> {
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, OrbitError> {
    let file = std::fs::File::open(path)
        .map_err(|error| OrbitError::Io(format!("open {}: {error}", path.display())))?;
    open_file_sha256(file, path)
}

fn open_file_sha256(mut file: std::fs::File, path: &Path) -> Result<String, OrbitError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// `sha256:<hex>` over `outputs` (§3.6).
pub fn plugin_artifact_digest(outputs: &[PluginBuildOutputRecord]) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(artifact_digest_preimage(outputs).as_bytes())
    )
}

/// The artifact digest of the outputs as installed under `install_path` now,
/// with their current modes: what doctor compares to the record. `Err`
/// names the output that cannot be read.
pub fn installed_artifact_digest(
    install_path: &Path,
    recorded: &[PluginBuildOutputRecord],
) -> Result<String, String> {
    let mut current = Vec::new();
    for output in recorded {
        let path = install_path.join(&output.to);
        let file = physical_output(install_path, &output.to).map_err(|error| error.to_string())?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("output '{}' is unreadable: {error}", output.to))?;
        if !metadata.is_file() {
            return Err(format!(
                "output '{}' is no longer a regular file",
                output.to
            ));
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o7777
        };
        #[cfg(not(unix))]
        let mode = output.mode;
        current.push(PluginBuildOutputRecord {
            to: output.to.clone(),
            mode,
            sha256: open_file_sha256(file, &path).map_err(|error| error.to_string())?,
        });
    }
    Ok(plugin_artifact_digest(&current))
}

/// Whether every declared output is already a regular file in `root`: the
/// requirement for a manifest with `spec.build` installed from a source that
/// never builds (§3.1).
pub fn prebuilt_outputs_present(root: &Path, outputs: &[PluginBuildOutput]) -> Result<(), String> {
    for output in outputs {
        let path = root.join(&output.to);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err(output.to.clone()),
        }
    }
    Ok(())
}
