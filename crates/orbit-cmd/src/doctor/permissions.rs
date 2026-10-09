use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::{OrbitError, OrbitRuntime};

/// Report Orbit-owned state directories whose write bits let another local
/// principal replace or unlink private files held beneath them.
pub(super) fn state_directory_permissions_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    #[cfg(unix)]
    {
        let scan =
            match inspect_state_directories(runtime) {
                Ok(scan) => scan,
                Err(error) => return WorkspaceDoctorResult {
                    duration_ms: 0,
                    check_name: "state-directory-permissions".to_string(),
                    status: WorkspaceDoctorStatus::Error,
                    message: error.to_string(),
                    remediation: Some(
                        "Fix the directory access error named above, then rerun `orbit doctor`."
                            .to_string(),
                    ),
                },
            };
        if scan.writable.is_empty() {
            return WorkspaceDoctorResult {
                duration_ms: 0,
                check_name: "state-directory-permissions".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: "all Orbit state directories deny group/world write access".to_string(),
                remediation: None,
            };
        }
        let locations = scan
            .offenders
            .iter()
            .map(|(path, count)| format!("{} ({count} writable)", path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "state-directory-permissions".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!("{} Orbit state director{} group/world writable under: {locations}",
                scan.writable.len(), if scan.writable.len() == 1 { "y is" } else { "ies are" }),
            remediation: Some("Run `orbit doctor --fix-state-directory-permissions` to restrict writable Orbit state directories to owner-only access, excluding run worktrees and target trees, then rerun `orbit doctor`.".to_string()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = runtime;
        WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "state-directory-permissions".to_string(),
            status: WorkspaceDoctorStatus::Skipped,
            message: "Unix directory mode checks are not available on this platform".to_string(),
            remediation: None,
        }
    }
}

/// Tighten only the directories reported by the read-only probe. Worktrees,
/// targets and child symlinks share the scanner's exclusions.
pub(super) fn repair_state_directory_permissions(
    runtime: &OrbitRuntime,
) -> Result<usize, OrbitError> {
    #[cfg(unix)]
    {
        let scan = inspect_state_directories(runtime)?;
        let mut repaired = 0;
        for (path, _) in scan.writable {
            repaired += usize::from(tighten_directory(&path).map_err(|error| {
                OrbitError::Io(format!(
                    "could not repair Orbit state directory '{}': {error}",
                    path.display()
                ))
            })?);
        }
        Ok(repaired)
    }
    #[cfg(not(unix))]
    {
        let _ = runtime;
        Err(OrbitError::InvalidInput(
            "Unix directory mode repairs are not available on this platform".to_string(),
        ))
    }
}

#[cfg(unix)]
struct PermissionScan {
    writable: Vec<(std::path::PathBuf, u32)>,
    offenders: Vec<(std::path::PathBuf, usize)>,
}

#[cfg(unix)]
fn inspect_state_directories(runtime: &OrbitRuntime) -> Result<PermissionScan, OrbitError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut scan = PermissionScan {
        writable: Vec::new(),
        offenders: Vec::new(),
    };
    // Only the two configured owner roots may resolve through links. Their
    // state/tasks/cache children are inspected as entries, just like deeper
    // children, so a linked subtree cannot redirect the scan or repair.
    for configured_owner in [runtime.global_root(), runtime.paths().orbit_dir.clone()] {
        let owner = match configured_owner.canonicalize() {
            Ok(owner) => owner,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "could not resolve Orbit state directory '{}': {error}",
                    configured_owner.display()
                )));
            }
        };
        let mut roots = vec![
            (owner.clone(), false),
            (owner.join("state"), true),
            (owner.join("tasks"), true),
            (owner.join("frictions"), true),
        ];
        if configured_owner == runtime.global_root() {
            roots.push((owner.join("cache"), true));
        }
        for (root, descend) in roots {
            let before = scan.writable.len();
            visit(&root, descend, &mut seen, &mut scan.writable, &mut |_| {}).map_err(|error| {
                OrbitError::Io(format!(
                    "could not inspect Orbit state directory '{}': {error}",
                    root.display()
                ))
            })?;
            let count = scan.writable.len() - before;
            if count > 0 {
                scan.offenders.push((root, count));
            }
        }
    }
    Ok(scan)
}

/// Open every component relative to the preceding directory descriptor. Neither
/// a final symlink nor a parent swapped after scanning can redirect the chmod.
#[cfg(unix)]
fn open_directory_no_follow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::path::Component;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory repair requires an absolute path",
        ));
    }
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open("/")?;
    for component in path.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => CString::new(name.as_bytes())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directory repair requires a canonical path",
                ));
            }
        };
        // SAFETY: directory owns a live descriptor and name is NUL-terminated.
        // O_DIRECTORY and O_NOFOLLOW bind only a real directory at this step.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new descriptor owned exclusively here.
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(unix)]
pub(super) fn tighten_directory(path: &std::path::Path) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let Some(directory) = present(open_directory_no_follow(path))? else {
        return Ok(false);
    };
    if directory.metadata()?.permissions().mode() & 0o022 == 0 {
        return Ok(false);
    }
    directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    Ok(true)
}

#[cfg(unix)]
fn excluded(path: &std::path::Path) -> bool {
    path.file_name().is_some_and(|name| name == "target")
        || (path.file_name().is_some_and(|name| name == "worktrees")
            && path
                .parent()
                .and_then(std::path::Path::file_name)
                .is_some_and(|name| name == "state"))
}

#[cfg(unix)]
fn present<T>(result: std::io::Result<T>) -> std::io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// No symlink traversal: worktree contents and Cargo output are outside Orbit's ownership.
/// The callback permits deterministic stat/read_dir race injection in sibling tests.
#[cfg(unix)]
fn visit(
    path: &std::path::Path,
    descend: bool,
    seen: &mut std::collections::BTreeSet<std::path::PathBuf>,
    writable: &mut Vec<(std::path::PathBuf, u32)>,
    before_read_dir: &mut impl FnMut(&std::path::Path),
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if excluded(path) || !seen.insert(path.to_path_buf()) {
        return Ok(());
    }
    let Some(metadata) = present(std::fs::symlink_metadata(path))? else {
        return Ok(());
    };
    if !metadata.is_dir() {
        return Ok(());
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        writable.push((path.to_path_buf(), mode));
    }
    if !descend {
        return Ok(());
    }
    before_read_dir(path);
    let Some(entries) = present(std::fs::read_dir(path))? else {
        // It vanished after stat; do not report a directory the operator cannot fix.
        if mode & 0o022 != 0 {
            writable.pop();
        }
        return Ok(());
    };
    for entry in entries {
        let Some(entry) = present(entry)? else {
            continue;
        };
        // DirEntry normally knows the type without another stat. NotFound at either
        // boundary is harmless when a concurrent process removes the directory.
        if present(entry.file_type())?.is_some_and(|kind| kind.is_dir()) {
            visit(&entry.path(), true, seen, writable, before_read_dir)?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
pub(super) type ScanReport = (
    std::collections::BTreeSet<std::path::PathBuf>,
    Vec<(std::path::PathBuf, u32)>,
);

/// Test seam for filesystem ownership and deterministic disappearance races.
#[cfg(all(test, unix))]
pub(super) fn scan_with_hook(
    path: &std::path::Path,
    before_read_dir: &mut impl FnMut(&std::path::Path),
) -> std::io::Result<ScanReport> {
    let mut seen = std::collections::BTreeSet::new();
    let mut writable = Vec::new();
    visit(path, true, &mut seen, &mut writable, before_read_dir)?;
    Ok((seen, writable))
}
