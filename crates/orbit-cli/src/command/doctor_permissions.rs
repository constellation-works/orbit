use orbit_cmd::{WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::OrbitRuntime;

/// Report Orbit-owned state directories whose write bits let another local
/// principal replace or unlink private files held beneath them.
pub(super) fn state_directory_permissions_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    #[cfg(unix)]
    {
        use std::collections::BTreeSet;

        let mut seen = BTreeSet::new();
        let mut writable = Vec::new();
        let mut offenders = Vec::new();
        let global = runtime.global_root();
        let workspace = runtime.paths().orbit_dir.clone();
        let configured_roots = [
            (global.clone(), false),
            (global.join("state"), true),
            (global.join("tasks"), true),
            (global.join("cache"), true),
            (global.join("frictions"), true),
            (workspace.clone(), false),
            (workspace.join("state"), true),
            (workspace.join("tasks"), true),
            (workspace.join("frictions"), true),
        ];
        for (configured_root, descend) in configured_roots {
            let root = match configured_root.canonicalize() {
                Ok(root) => root,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return WorkspaceDoctorResult {
                        duration_ms: 0,
                        check_name: "state-directory-permissions".to_string(),
                        status: WorkspaceDoctorStatus::Error,
                        message: format!(
                            "could not resolve Orbit state directory '{}': {error}",
                            configured_root.display()
                        ),
                        remediation: Some(
                            "Fix the directory access error named above, then rerun `orbit doctor`."
                                .to_string(),
                        ),
                    };
                }
            };
            let before = writable.len();
            if let Err(error) = visit(&root, descend, &mut seen, &mut writable, &mut |_| {}) {
                return WorkspaceDoctorResult {
                    duration_ms: 0,
                    check_name: "state-directory-permissions".to_string(),
                    status: WorkspaceDoctorStatus::Error,
                    message: format!(
                        "could not inspect Orbit state directory '{}': {error}",
                        root.display()
                    ),
                    remediation: Some(
                        "Fix the directory access error named above, then rerun `orbit doctor`."
                            .to_string(),
                    ),
                };
            }
            let count = writable.len() - before;
            if count > 0 {
                offenders.push((root, count));
            }
        }

        if writable.is_empty() {
            return WorkspaceDoctorResult {
                duration_ms: 0,
                check_name: "state-directory-permissions".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: "all Orbit state directories deny group/world write access".to_string(),
                remediation: None,
            };
        }

        let locations = offenders
            .iter()
            .map(|(path, count)| format!("{} ({count} writable)", path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "state-directory-permissions".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!(
                "{} Orbit state director{} group/world writable under: {locations}",
                writable.len(),
                if writable.len() == 1 {
                    "y is"
                } else {
                    "ies are"
                }
            ),
            remediation: Some(
                "Remove group/world write permission from the writable directories under each named root \
                 (for example, `chmod go-w <directory>`), excluding run worktrees and target trees, \
                 then rerun `orbit doctor`."
                    .to_string(),
            ),
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

    if path.file_name().is_some_and(|name| name == "target") || !seen.insert(path.to_path_buf()) {
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
    if !descend
        || (path.file_name().is_some_and(|name| name == "worktrees")
            && path
                .parent()
                .and_then(std::path::Path::file_name)
                .is_some_and(|name| name == "state"))
    {
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
