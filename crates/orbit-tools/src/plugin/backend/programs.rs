use super::*;

/// One `requires.programs` entry as the host will treat it on the next call:
/// the absolute path recorded when the operator enabled the plugin, and why
/// that path will not be granted, when it will not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProgramStatus {
    /// The program as the manifest declares it (`uv`, `/opt/tool/bin/tool`).
    pub name: String,
    /// The canonical path recorded at enable time, `None` when nothing was
    /// recorded for this name.
    pub path: Option<PathBuf>,
    /// Why the sandbox will not grant this program, `None` when it will.
    pub problem: Option<String>,
    /// The file this bare name resolves to on the inspecting `PATH`, when that
    /// is not the recorded file (an auto-updated CLI, say). Set only by
    /// [`mark_resolution_drift`] for inspection; the sandbox and the child
    /// never read it, so the spawning `PATH` cannot change what runs.
    pub resolves_to: Option<PathBuf>,
}

impl PluginProgramStatus {
    pub fn granted(&self) -> bool {
        self.path.is_some() && self.problem.is_none()
    }
}

/// Whether a declared entry is a bare name the child finds on its `PATH`
/// (`uv`), rather than an absolute path it runs as written.
fn is_bare_program_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\0']) && name != "." && name != ".."
}

/// Flag each granted bare name whose lookup on `search_path` now reaches a
/// different file than the recorded one. Inspection only (`orbit plugin show`,
/// `orbit plugin doctor`): the plugin keeps running the recorded file, and a
/// changed resolution is handled by re-consent, never followed silently.
pub fn mark_resolution_drift(statuses: &mut [PluginProgramStatus], search_path: Option<&OsStr>) {
    for status in statuses {
        let Some(recorded) = status.path.as_deref() else {
            continue;
        };
        if !is_bare_program_name(&status.name) {
            continue;
        }
        status.resolves_to = match resolve_declared_program(&status.name, search_path) {
            Ok(current) if current != recorded => Some(current),
            _ => None,
        };
    }
}

/// Resolve every declared program against `search_path` — the consenting
/// operator's `PATH` at `orbit plugin enable` — into the paths to record, plus
/// each entry that did not resolve and why.
///
/// Resolution happens once, at consent, and never against a caller's `PATH`:
/// a systemd unit, the MCP server and an ssh shell spawn the same backend
/// with different `PATH`s, and the program a plugin may execute must not
/// depend on which of them spawned it.
pub fn resolve_declared_programs(
    declared: &[String],
    search_path: Option<&OsStr>,
) -> (BTreeMap<String, PathBuf>, Vec<(String, String)>) {
    let mut resolved = BTreeMap::new();
    let mut unresolved = Vec::new();
    for program in declared {
        match resolve_declared_program(program, search_path) {
            Ok(path) => {
                resolved.insert(program.clone(), path);
            }
            Err(reason) => unresolved.push((program.clone(), reason)),
        }
    }
    (resolved, unresolved)
}

/// Resolve one declared program: an absolute path as written, a bare name
/// through `search_path` the way a shell would, and either way to the
/// canonical executable file a Landlock rule binds.
///
/// Recording the canonical path means a symbolic link retargeted after
/// consent is not followed silently: the recorded path stops matching and
/// the program is not granted until the operator re-enables the plugin.
pub fn resolve_declared_program(
    program: &str,
    search_path: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let program = program.trim();
    if program.is_empty() {
        return Err("the entry is empty".to_string());
    }
    if program.contains('/') {
        let path = Path::new(program);
        if !path.is_absolute() {
            return Err(format!(
                "`{program}` is a relative path; declare a program name looked up on PATH, or \
                 an absolute path"
            ));
        }
        return canonical_executable(path);
    }
    let Some(search_path) = search_path else {
        return Err("PATH is not set in the enabling environment".to_string());
    };
    std::env::split_paths(search_path)
        // A relative `PATH` entry resolves against whatever directory the
        // enabling shell happened to be in, which is not a location anyone
        // consented to.
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| executable_file_problem(candidate).is_none())
        .map_or_else(
            || Err(format!("`{program}` is not an executable file on PATH")),
            |candidate| canonical_executable(&candidate),
        )
}

/// The status of every declared program given what was recorded for it.
///
/// A recorded path is granted only while it still names the executable the
/// operator consented to: it must exist, be an executable regular file, and
/// still be its own canonical path. It is also never granted inside a
/// host-owned tree (`PLUGIN_GLOBAL_READ_DENY_DIRS`) — a Landlock read rule
/// on a single file inside a denied directory would hand the child that
/// file, so a program there could buy back another plugin's state.
pub fn program_statuses(
    declared: &[String],
    recorded: &BTreeMap<String, PathBuf>,
    global_root: &Path,
    state_dir: &Path,
) -> Vec<PluginProgramStatus> {
    declared
        .iter()
        .map(|name| {
            let path = recorded.get(name).cloned();
            let problem = match &path {
                None => Some(
                    "no path was recorded for it when the plugin was enabled (it did not resolve \
                     to an executable on the enabling PATH, or an older Orbit enabled the plugin)"
                        .to_string(),
                ),
                Some(path) => recorded_program_problem(path, global_root, state_dir),
            };
            PluginProgramStatus {
                name: name.clone(),
                path,
                problem,
                resolves_to: None,
            }
        })
        .collect()
}

impl PluginBackendSpec {
    /// The host-owned directory of this plugin's program links.
    fn program_link_dir(&self) -> PathBuf {
        self.global_root
            .join(plugin_program_links_relative(&self.provenance.name))
    }

    /// Granted bare program names with the recorded file each one links to.
    /// Absolute declared entries run as written and need no link.
    fn linkable_programs(&self) -> Vec<(String, PathBuf)> {
        self.program_statuses()
            .into_iter()
            .filter(|status| status.granted() && is_bare_program_name(&status.name))
            .filter_map(|status| status.path.map(|path| (status.name, path)))
            .collect()
    }

    /// The directory the child's `PATH` gets prepended with, when at least one
    /// granted program is reachable by its declared name. Reads no link; the
    /// sandbox grants read on it so the child can look names up in it.
    pub(crate) fn granted_program_dir(&self) -> Option<PathBuf> {
        (!self.linkable_programs().is_empty()).then(|| self.program_link_dir())
    }

    /// Make the program link directory hold exactly one link per granted
    /// program name, each pointing at its recorded path, and remove every
    /// other link (a name the manifest no longer declares or no longer
    /// grants). Returns the directory when it holds at least one link.
    ///
    /// The directory lives under `plugins/`, which the plugin may read but
    /// never write, so only this host call changes what a granted name runs.
    pub(crate) fn materialize_program_links(&self) -> Result<Option<PathBuf>, OrbitError> {
        let programs = self.linkable_programs();
        let dir = self.program_link_dir();
        let wanted: Vec<&str> = programs.iter().map(|(name, _)| name.as_str()).collect();
        if programs.is_empty() {
            remove_program_links_except(&dir, &wanted)?;
            return Ok(None);
        }
        orbit_common::fs::io::create_private_dir_all(&dir)
            .map_err(|error| link_error(&dir, &error))?;
        for (name, target) in &programs {
            replace_program_link(&dir, name, target)?;
        }
        remove_program_links_except(&dir, &wanted)?;
        Ok(Some(dir))
    }
}

fn link_error(path: &Path, error: &std::io::Error) -> OrbitError {
    OrbitError::Execution(format!(
        "plugin program links at {}: {error}",
        path.display()
    ))
}

/// Point `dir/name` at `target`, replacing whatever link was there. The new
/// link is staged beside it and renamed over, so a child never sees a missing
/// name in between.
#[cfg(unix)]
fn replace_program_link(dir: &Path, name: &str, target: &Path) -> Result<(), OrbitError> {
    let link = dir.join(name);
    if std::fs::read_link(&link).is_ok_and(|current| current == target) {
        return Ok(());
    }
    let staging = dir.join(format!(".{name}.staging"));
    remove_path_if_present(&staging).map_err(|error| link_error(&staging, &error))?;
    std::os::unix::fs::symlink(target, &staging).map_err(|error| link_error(&staging, &error))?;
    std::fs::rename(&staging, &link).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        link_error(&link, &error)
    })
}

#[cfg(not(unix))]
fn replace_program_link(_dir: &Path, name: &str, _target: &Path) -> Result<(), OrbitError> {
    Err(OrbitError::Execution(format!(
        "plugin program `{name}` needs symbolic links, which this platform does not provide"
    )))
}

/// Remove every entry of `dir` that is not one of `keep`, and nothing else:
/// only symbolic links are host links, so a directory or file is an error,
/// not something to delete. A missing directory has nothing to remove.
fn remove_program_links_except(dir: &Path, keep: &[&str]) -> Result<(), OrbitError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(link_error(dir, &error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| link_error(dir, &error))?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| keep.contains(&name)) {
            continue;
        }
        let path = entry.path();
        let is_link = entry
            .file_type()
            .map_err(|error| link_error(&path, &error))?
            .is_symlink();
        if !is_link {
            return Err(OrbitError::Execution(format!(
                "plugin program links at {} hold {}, which is not a host link",
                dir.display(),
                path.display()
            )));
        }
        remove_path_if_present(&path).map_err(|error| link_error(&path, &error))?;
    }
    Ok(())
}

fn remove_path_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn recorded_program_problem(path: &Path, global_root: &Path, state_dir: &Path) -> Option<String> {
    if !path.is_absolute() {
        return Some(format!(
            "its recorded path {} is not absolute",
            path.display()
        ));
    }
    if let Some(problem) = executable_file_problem(path) {
        return Some(format!("its recorded path {}: {problem}", path.display()));
    }
    match path.canonicalize() {
        Ok(canonical) if canonical == path => {}
        Ok(canonical) => {
            return Some(format!(
                "its recorded path {} now resolves to {}",
                path.display(),
                canonical.display()
            ));
        }
        Err(error) => {
            return Some(format!(
                "its recorded path {} cannot be resolved: {error}",
                path.display()
            ));
        }
    }
    let own_state = physical_with_missing_tail(state_dir);
    let inside_denied = !path.starts_with(&own_state)
        && PLUGIN_GLOBAL_READ_DENY_DIRS.iter().any(|relative| {
            path.starts_with(physical_with_missing_tail(&global_root.join(relative)))
        });
    inside_denied.then(|| {
        format!(
            "its recorded path {} is inside host-owned Orbit state, which no plugin may read",
            path.display()
        )
    })
}

fn canonical_executable(path: &Path) -> Result<PathBuf, String> {
    if let Some(problem) = executable_file_problem(path) {
        return Err(format!("{}: {problem}", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Why `path` (following links) is not an executable regular file.
fn executable_file_problem(path: &Path) -> Option<String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some("no such file".to_string());
        }
        Err(error) => return Some(error.to_string()),
    };
    if !metadata.is_file() {
        return Some("not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Some("not executable".to_string());
        }
    }
    None
}
