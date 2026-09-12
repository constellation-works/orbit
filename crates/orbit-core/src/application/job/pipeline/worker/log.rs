use super::*;

const PIPELINE_WORKER_LOG_TAIL_BYTES: u64 = 16 * 1024;

pub(crate) fn pipeline_worker_log_path(
    logs_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    Ok(logs_dir.join(pipeline_worker_file_name(run_id, ".worker.log")?))
}

/// Turn a persisted run ID into a filename only after rejecting path syntax.
///
/// Run IDs normally come from the store, but worker entry points also accept
/// an ID from a process argument. Keeping this check at the shared filename
/// boundary prevents either source from steering pipeline artifacts outside
/// their owning directory.
pub(super) fn pipeline_worker_file_name(run_id: &str, suffix: &str) -> Result<String, OrbitError> {
    let safe = !run_id.is_empty()
        && run_id != "."
        && run_id != ".."
        && !run_id.contains(['/', '\\'])
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !safe {
        return Err(OrbitError::InvalidInput(format!(
            "job run id must be a safe filename stem: {run_id}"
        )));
    }

    Ok(format!("{run_id}{suffix}"))
}

/// Resolve the worker-log directory before using it for any file operation.
///
/// Containment is the nearest existing parent, canonicalized, plus the final
/// component. Ancestor symlinks are followed rather than rejected so ordinary
/// layouts (a symlinked `/tmp`, `$HOME`, or project root) can still spawn a
/// worker. A missing parent is rebuilt from that canonical ancestor so the
/// caller can create intermediate directories. The final component itself
/// must not be a symlink or a non-directory; traversal syntax fails closed.
#[cfg(not(unix))]
fn validated_pipeline_worker_log_directory(path: &Path) -> Result<PathBuf, OrbitError> {
    let validated_path = validate_pipeline_worker_log_directory_input(path)?;
    let parent = validated_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no parent: {}",
            path.display()
        ))
    })?;
    let file_name = validated_path.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no final component: {}",
            path.display()
        ))
    })?;
    let canonical_parent = canonical_pipeline_worker_log_parent(parent)?;
    let canonical_path = canonical_parent.join(file_name);
    if !canonical_path.starts_with(&canonical_parent) {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log directory must not contain traversal components: {}",
            path.display()
        )));
    }

    match std::fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(OrbitError::InvalidInput(format!(
                "pipeline worker log directory must not be a symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(OrbitError::InvalidInput(format!(
                "pipeline worker log path is not a directory: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect pipeline worker log directory '{}': {error}",
                path.display()
            )));
        }
    }

    Ok(canonical_path)
}

#[cfg(unix)]
struct PipelineWorkerLogDirectory {
    path: PathBuf,
    directory: File,
}

/// Open the validated authority and create the log directory beneath its fd.
///
/// The trusted authority is the inode of the nearest existing ancestor after
/// resolving aliases which already existed when setup began. That preserves
/// supported aliases such as a symlinked `/tmp`, home, or project root. The
/// inode is checked while opening it, then every missing suffix component is
/// created and opened relative to the held descriptor with symlink following
/// disabled. Renames after that point cannot redirect creation, open, or chmod
/// to another filesystem object.
#[cfg(unix)]
fn prepare_pipeline_worker_log_directory(
    path: &Path,
) -> Result<PipelineWorkerLogDirectory, OrbitError> {
    let validated_path = validate_pipeline_worker_log_directory_input(path)?;
    let parent = validated_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no parent: {}",
            path.display()
        ))
    })?;
    let final_name = validated_path.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log directory has no final component: {}",
            path.display()
        ))
    })?;
    let (authority_path, missing, expected_authority) =
        canonical_pipeline_worker_log_parent_components(parent)?;

    #[cfg(all(test, unix))]
    pipeline_worker_log_test_hook::run(
        pipeline_worker_log_test_hook::Phase::AuthorityValidated,
        &authority_path,
    );

    let mut directory = open_pipeline_worker_directory(None, &authority_path)?;
    let opened_authority = directory.metadata().map_err(|error| {
        OrbitError::Io(format!(
            "inspect opened pipeline worker log authority '{}': {error}",
            authority_path.display()
        ))
    })?;
    if expected_authority.dev() != opened_authority.dev()
        || expected_authority.ino() != opened_authority.ino()
    {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log authority changed while it was opened: {}",
            authority_path.display()
        )));
    }

    let mut resolved_path = authority_path;
    for component in missing
        .iter()
        .map(OsString::as_os_str)
        .chain(std::iter::once(final_name))
    {
        resolved_path.push(component);
        create_pipeline_worker_directory_at(&directory, component, &resolved_path)?;
        directory = open_pipeline_worker_directory(Some(&directory), Path::new(component))?;
    }

    Ok(PipelineWorkerLogDirectory {
        path: resolved_path,
        directory,
    })
}

#[cfg(unix)]
fn canonical_pipeline_worker_log_parent_components(
    parent: &Path,
) -> Result<(PathBuf, Vec<OsString>, std::fs::Metadata), OrbitError> {
    let mut existing = parent.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match std::fs::metadata(&existing) {
            Ok(metadata) => {
                let canonical = existing.canonicalize().map_err(|error| {
                    OrbitError::Io(format!(
                        "resolve pipeline worker log authority '{}': {error}",
                        existing.display()
                    ))
                })?;
                missing.reverse();
                return Ok((canonical, missing, metadata));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                }
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect pipeline worker log directory '{}': {error}",
                    existing.display()
                )));
            }
        }
    }
}

#[cfg(unix)]
fn pipeline_worker_component_c_string(component: &OsStr) -> Result<std::ffi::CString, OrbitError> {
    std::ffi::CString::new(component.as_bytes()).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "pipeline worker log path contains an invalid null byte: {}",
            component.to_string_lossy()
        ))
    })
}

#[cfg(unix)]
fn open_pipeline_worker_directory(parent: Option<&File>, path: &Path) -> Result<File, OrbitError> {
    let path_c = pipeline_worker_component_c_string(path.as_os_str())?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = match parent {
        Some(parent) => unsafe { libc::openat(parent.as_raw_fd(), path_c.as_ptr(), flags) },
        None => unsafe { libc::open(path_c.as_ptr(), flags) },
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
        {
            return Err(OrbitError::InvalidInput(format!(
                "pipeline worker log directory must not be a symlink or non-directory: {}",
                path.display()
            )));
        }
        return Err(OrbitError::Io(format!(
            "open pipeline worker log directory '{}': {error}",
            path.display(),
        )));
    }

    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn create_pipeline_worker_directory_at(
    parent: &File,
    component: &OsStr,
    display_path: &Path,
) -> Result<(), OrbitError> {
    let component_c = pipeline_worker_component_c_string(component)?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), component_c.as_ptr(), 0o700) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(OrbitError::Io(format!(
                "create pipeline worker log directory '{}': {error}",
                display_path.display()
            )));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn open_pipeline_worker_log_at(
    directory: &File,
    file_name: &OsStr,
    display_path: &Path,
) -> Result<File, OrbitError> {
    let file_name_c = pipeline_worker_component_c_string(file_name)?;
    let flags = libc::O_CREAT | libc::O_APPEND | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), file_name_c.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(OrbitError::Io(format!(
            "open pipeline worker log '{}': {}",
            display_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(all(test, unix))]
pub(crate) mod pipeline_worker_log_test_hook {
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Phase {
        AuthorityValidated,
        DirectoryReady,
        BeforeLogOpen,
    }

    type Hook = Box<dyn FnOnce(&Path)>;

    thread_local! {
        static HOOK: RefCell<Option<(Phase, Hook)>> = RefCell::new(None);
    }

    pub(crate) fn install<F>(phase: Phase, hook: F)
    where
        F: FnOnce(&Path) + 'static,
    {
        HOOK.with(|slot| *slot.borrow_mut() = Some((phase, Box::new(hook))));
    }

    pub(crate) fn clear() {
        HOOK.with(|slot| *slot.borrow_mut() = None);
    }

    pub(super) fn run(phase: Phase, path: &Path) {
        let hook = HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(expected, _)| *expected == phase)
            {
                slot.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook(path);
        }
    }
}

/// Perform path-shape checks before the value reaches filesystem APIs.
///
/// The returned value is the only path accepted by the filesystem-resolution
/// phase below. Keeping this phase free of filesystem operations makes the
/// trust boundary explicit to both reviewers and CodeQL.
fn validate_pipeline_worker_log_directory_input(path: &Path) -> Result<PathBuf, OrbitError> {
    if pipeline_worker_log_directory_input_is_valid(path) {
        return Ok(path.to_path_buf());
    }

    if !path.is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "pipeline worker log directory must be absolute: {}",
            path.display()
        )));
    }

    Err(OrbitError::InvalidInput(format!(
        "pipeline worker log directory must not contain traversal components: {}",
        path.display()
    )))
}

/// Report whether a worker-log directory has the lexical shape required by
/// the filesystem-resolution phase.
///
/// This boolean guard is kept separate because CodeQL's Rust model API can
/// represent conditional validation directly, unlike a successful `Result`
/// projection.
fn pipeline_worker_log_directory_input_is_valid(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

/// Canonicalize the nearest existing ancestor of `parent` and rejoin any
/// missing suffix. Unrelated ancestor symlinks are resolved; a dangling
/// symlink fails closed when canonicalization reaches that component.
#[cfg(not(unix))]
fn canonical_pipeline_worker_log_parent(parent: &Path) -> Result<PathBuf, OrbitError> {
    let mut existing = parent.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match existing.canonicalize() {
            Ok(mut canonical) => {
                for name in missing.into_iter().rev() {
                    canonical.push(name);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Err(OrbitError::InvalidInput(format!(
                        "pipeline worker log directory has no parent: {}",
                        parent.display()
                    )));
                }
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect pipeline worker log directory '{}': {error}",
                    existing.display()
                )));
            }
        }
    }
}

pub(crate) fn configure_pipeline_worker_stdio(
    command: &mut Command,
    logs_dir: &Path,
    run_id: &str,
) -> Result<PipelineWorkerLog, OrbitError> {
    #[cfg(unix)]
    let PipelineWorkerLogDirectory {
        path: logs_dir,
        directory,
    } = prepare_pipeline_worker_log_directory(logs_dir)?;

    #[cfg(not(unix))]
    let logs_dir = {
        let logs_dir = validated_pipeline_worker_log_directory(logs_dir)?;
        std::fs::create_dir_all(&logs_dir).map_err(|error| {
            OrbitError::Io(format!(
                "create pipeline worker log directory '{}': {error}",
                logs_dir.display()
            ))
        })?;
        validated_pipeline_worker_log_directory(&logs_dir)?
    };
    let log_path = pipeline_worker_log_path(&logs_dir, run_id)?;

    #[cfg(all(test, unix))]
    pipeline_worker_log_test_hook::run(
        pipeline_worker_log_test_hook::Phase::DirectoryReady,
        &logs_dir,
    );

    #[cfg(unix)]
    restrict_pipeline_worker_log_directory(&directory, &logs_dir)?;
    #[cfg(not(unix))]
    restrict_pipeline_worker_log_directory(&logs_dir)?;

    #[cfg(all(test, unix))]
    pipeline_worker_log_test_hook::run(
        pipeline_worker_log_test_hook::Phase::BeforeLogOpen,
        &log_path,
    );

    #[cfg(unix)]
    let mut log = open_pipeline_worker_log_at(
        &directory,
        log_path.file_name().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "pipeline worker log has no final component: {}",
                log_path.display()
            ))
        })?,
        &log_path,
    )?;
    #[cfg(not(unix))]
    let mut options = OpenOptions::new();
    #[cfg(not(unix))]
    options.create(true).append(true).read(true);
    #[cfg(not(unix))]
    let mut log = options.open(&log_path).map_err(|error| {
        OrbitError::Io(format!(
            "open pipeline worker log '{}': {error}",
            log_path.display()
        ))
    })?;
    #[cfg(unix)]
    restrict_pipeline_worker_log_file(&log, &log_path)?;
    #[cfg(not(unix))]
    restrict_pipeline_worker_log_file(&log_path)?;
    if let Some(profile) = pipeline_worker_profile_file(
        &logs_dir,
        run_id,
        std::env::var_os("LLVM_PROFILE_FILE").as_deref(),
    )? {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    write_pipeline_worker_spawn_banner(&mut log, command);
    let reader = log.try_clone().map_err(|error| {
        OrbitError::Io(format!(
            "clone pipeline worker log reader '{}': {error}",
            log_path.display()
        ))
    })?;
    let stdout = log.try_clone().map_err(|error| {
        OrbitError::Io(format!(
            "clone pipeline worker log '{}': {error}",
            log_path.display()
        ))
    })?;
    command.stdout(Stdio::from(stdout)).stderr(Stdio::from(log));
    Ok(PipelineWorkerLog {
        path: log_path,
        reader,
    })
}

pub(crate) struct PipelineWorkerLog {
    pub(super) path: PathBuf,
    pub(super) reader: File,
}

impl PipelineWorkerLog {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn write_pipeline_worker_spawn_banner(file: &mut File, command: &Command) {
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let cwd = command.get_current_dir().map_or_else(
        || "<inherit>".to_string(),
        |path| path.display().to_string(),
    );
    let _ = writeln!(
        file,
        "orbit pipeline worker spawn\nprogram: {}\nargs: {args}\ncwd: {cwd}",
        command.get_program().to_string_lossy()
    );
}

pub(super) fn read_pipeline_worker_log_tail(file: &mut File) -> Option<String> {
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(PIPELINE_WORKER_LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let output = String::from_utf8_lossy(&bytes);
    let output = orbit_common::observability::logging::redact_event_text(output.trim());
    if output.is_empty() {
        None
    } else if start > 0 {
        Some(format!(
            "[truncated to final {PIPELINE_WORKER_LOG_TAIL_BYTES} bytes]\n{output}"
        ))
    } else {
        Some(output)
    }
}

#[cfg(unix)]
fn restrict_pipeline_worker_log_directory(directory: &File, path: &Path) -> Result<(), OrbitError> {
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } < 0 {
        let error = std::io::Error::last_os_error();
        Err(OrbitError::Io(format!(
            "restrict pipeline worker log directory '{}': {error}",
            path.display()
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn restrict_pipeline_worker_log_directory(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_pipeline_worker_log_file(file: &File, path: &Path) -> Result<(), OrbitError> {
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
        let error = std::io::Error::last_os_error();
        Err(OrbitError::Io(format!(
            "restrict pipeline worker log '{}': {error}",
            path.display()
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn restrict_pipeline_worker_log_file(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}
