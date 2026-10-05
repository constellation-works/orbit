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
}

impl PluginProgramStatus {
    pub fn granted(&self) -> bool {
        self.path.is_some() && self.problem.is_none()
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
/// host-owned tree ([`PLUGIN_GLOBAL_READ_DENY_DIRS`]) — a Landlock read rule
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
            }
        })
        .collect()
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
