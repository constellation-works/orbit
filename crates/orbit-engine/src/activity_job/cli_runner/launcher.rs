//! Provider launcher lookup and the agent tool environment pinned to the
//! dispatching Orbit build.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::spawn::SpawnError;

const ORBIT_BIN_ENV: &str = "ORBIT_BIN";

/// Conventional `$HOME` bin directories searched when a provider launcher
/// is not on the inherited `PATH`, and backfilled into a spawned agent's
/// `PATH` when those entries are absent. Keep this list shared so lookup
/// and child-env construction cannot drift. [ORB-10909]
const CONVENTIONAL_HOME_BIN_DIRS: &[&str] = &[".local/bin", ".orbit/bin", ".cargo/bin", "bin"];

/// Portable supported-launcher prefixes searched after `PATH` and `$HOME`
/// bins, and backfilled into the spawned agent `PATH`. These are OS package
/// prefixes, not user directories: Apple Silicon Homebrew, then the Intel
/// Homebrew / `/usr/local` prefix. launchd's default `PATH` omits both, which
/// is why a scheduled Mac drain cannot find `/opt/homebrew/bin/codex` while
/// an interactive shell can. [ORB-11808]
pub(crate) const SUPPORTED_SYSTEM_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Resolve a provider launcher without relying solely on the parent process's
/// ambient `PATH`.
///
/// Every CLI-backed provider enters through this resolver so service, routine,
/// dashboard, and interactive dispatches use the same lookup policy.
pub(crate) fn resolve_provider_launcher(
    provider: &str,
    program: &str,
    cwd: Option<&Path>,
) -> Result<String, SpawnError> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_provider_launcher_with(provider, program, path.as_deref(), home.as_deref(), cwd)
}

/// Where dispatch would launch `program` from, or `None` when it would fail
/// to find a launchable file. Read-only view of [`resolve_provider_launcher`]
/// for `orbit doctor providers`, so the diagnostic and dispatch share one
/// lookup policy.
pub fn locate_provider_launcher(program: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let resolved = PathBuf::from(resolve_provider_launcher(program, program, cwd).ok()?);
    let resolved = match cwd {
        Some(cwd) if resolved.is_relative() => cwd.join(resolved),
        _ => resolved,
    };
    is_launchable_file(&resolved).then_some(resolved)
}

/// Pin tools invoked by an agent to the Orbit build that dispatched it.
///
/// Long-lived services may retain a `PATH` whose first `orbit` is an older
/// Cargo install even after the operator deploys `~/.orbit/bin/orbit`. Export
/// the selected binary for hook scripts and put its directory first for bare
/// `orbit tool ...` invocations inside the provider (including Bubblewrap).
pub(crate) fn orbit_tool_env() -> Result<Vec<(String, String)>, SpawnError> {
    let current_exe = std::env::current_exe().map_err(|error| {
        SpawnError::permanent(format!(
            "resolve dispatching Orbit executable for agent tool environment: {error}"
        ))
    })?;
    let configured = std::env::var_os(ORBIT_BIN_ENV);
    let inherited_path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    orbit_tool_env_with(
        configured.as_deref(),
        &current_exe,
        inherited_path.as_deref(),
        home.as_deref(),
    )
}

// pub(crate) widened for sibling tests under the repository's enforced test layout.
pub(crate) fn orbit_tool_env_with(
    configured: Option<&OsStr>,
    current_exe: &Path,
    inherited_path: Option<&OsStr>,
    home: Option<&Path>,
) -> Result<Vec<(String, String)>, SpawnError> {
    let selected = configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| current_exe.to_path_buf());
    let selected_text = selected.to_string_lossy().into_owned();

    let Some(bin_dir) = selected
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(vec![(ORBIT_BIN_ENV.to_string(), selected_text)]);
    };

    let mut path_entries = vec![bin_dir.to_path_buf()];
    let mut seen = HashSet::new();
    seen.insert(bin_dir.to_path_buf());
    if let Some(inherited_path) = inherited_path {
        for entry in std::env::split_paths(inherited_path) {
            if seen.insert(entry.clone()) {
                path_entries.push(entry);
            }
        }
    }
    // Service/routine dispatch often inherits a PATH that omits cargo and
    // other login-shell bins. Backfill the same conventional home dirs and
    // supported system prefixes `resolve_provider_launcher_with` already
    // searches so `cargo test` and Homebrew-installed tools inside the
    // spawned agent are not "command not found". [ORB-10909] [ORB-11808]
    if let Some(home) = home {
        for dir in conventional_home_bin_dirs(home) {
            if seen.insert(dir.clone()) {
                path_entries.push(dir);
            }
        }
    }
    for dir in supported_system_bin_dirs() {
        if seen.insert(dir.clone()) {
            path_entries.push(dir);
        }
    }
    let pinned_path = std::env::join_paths(path_entries)
        .map_err(|error| {
            SpawnError::permanent(format!(
                "construct agent PATH pinned to `{}`: {error}",
                selected.display()
            ))
        })?
        .into_string()
        .map_err(|_| {
            SpawnError::permanent(format!(
                "agent PATH pinned to `{}` is not valid Unicode",
                selected.display()
            ))
        })?;

    Ok(vec![
        (ORBIT_BIN_ENV.to_string(), selected_text),
        ("PATH".to_string(), pinned_path),
    ])
}

fn conventional_home_bin_dirs(home: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    CONVENTIONAL_HOME_BIN_DIRS
        .iter()
        .map(|relative| home.join(relative))
}

fn supported_system_bin_dirs() -> impl Iterator<Item = PathBuf> {
    SUPPORTED_SYSTEM_BIN_DIRS.iter().map(PathBuf::from)
}

fn is_launchable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// pub(crate) widened for sibling tests under the repository's enforced test layout.
pub(crate) fn resolve_provider_launcher_with(
    provider: &str,
    program: &str,
    path: Option<&OsStr>,
    home: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<String, SpawnError> {
    resolve_provider_launcher_with_extra_dirs(
        provider,
        program,
        path,
        home,
        cwd,
        supported_system_bin_dirs(),
    )
}

/// Test-injectable resolver: production calls
/// [`resolve_provider_launcher_with`], which supplies the supported system
/// prefixes. Tests pass a temporary Homebrew-style prefix rather than
/// writing into `/opt/homebrew/bin`.
pub(crate) fn resolve_provider_launcher_with_extra_dirs(
    provider: &str,
    program: &str,
    path: Option<&OsStr>,
    home: Option<&Path>,
    cwd: Option<&Path>,
    extra_bin_dirs: impl IntoIterator<Item = PathBuf>,
) -> Result<String, SpawnError> {
    let configured = Path::new(program);
    if configured.components().count() > 1 {
        return Ok(program.to_string());
    }

    let mut search_dirs = Vec::new();
    let mut seen = HashSet::new();
    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            let dir = if dir.is_relative() {
                cwd.map_or(dir.clone(), |cwd| cwd.join(&dir))
            } else {
                dir
            };
            if seen.insert(dir.clone()) {
                search_dirs.push(dir);
            }
        }
    }
    if let Some(home) = home {
        for dir in conventional_home_bin_dirs(home) {
            if seen.insert(dir.clone()) {
                search_dirs.push(dir);
            }
        }
    }
    for dir in extra_bin_dirs {
        if seen.insert(dir.clone()) {
            search_dirs.push(dir);
        }
    }

    let mut searched = Vec::with_capacity(search_dirs.len());
    for dir in search_dirs {
        let candidate = dir.join(program);
        searched.push(candidate.clone());
        if is_launchable_file(&candidate) {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        if configured.extension().is_none() {
            for extension in windows_executable_extensions() {
                let candidate = dir.join(format!("{program}{extension}"));
                searched.push(candidate.clone());
                if is_launchable_file(&candidate) {
                    return Ok(candidate.to_string_lossy().into_owned());
                }
            }
        }
    }

    let searched = if searched.is_empty() {
        "<no PATH or HOME search locations available>".to_string()
    } else {
        searched
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(SpawnError::permanent(format!(
        "{}; searched: {searched}",
        missing_launcher_message(program, provider)
    )))
}

/// A provider launcher that dispatch could not find, as named by the
/// permanent spawn error [`resolve_provider_launcher`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingLauncher {
    pub program: String,
    pub provider: String,
}

const MISSING_LAUNCHER_PREFIX: &str = "provider launcher `";
const MISSING_LAUNCHER_PROVIDER: &str = "` for provider `";
const MISSING_LAUNCHER_SUFFIX: &str = "` was not found";

fn missing_launcher_message(program: &str, provider: &str) -> String {
    format!(
        "{MISSING_LAUNCHER_PREFIX}{program}{MISSING_LAUNCHER_PROVIDER}{provider}{MISSING_LAUNCHER_SUFFIX}"
    )
}

/// The launcher a "provider launcher not found" failure names, wherever that
/// error appears in `text` — a run step's error message or the task-history
/// note that quotes it. The failure is permanent for its run but not for its
/// task: installing the launcher fixes it, so callers that re-check blocked
/// tasks need the program back out of the recorded text. Parsed here, beside
/// [`missing_launcher_message`], so the format and its reader cannot drift.
pub fn missing_launcher_in(text: &str) -> Option<MissingLauncher> {
    let (_, rest) = text.split_once(MISSING_LAUNCHER_PREFIX)?;
    let (program, rest) = rest.split_once(MISSING_LAUNCHER_PROVIDER)?;
    let (provider, _) = rest.split_once(MISSING_LAUNCHER_SUFFIX)?;
    if program.is_empty() || program.contains('`') || provider.contains('`') {
        return None;
    }
    Some(MissingLauncher {
        program: program.to_string(),
        provider: provider.to_string(),
    })
}

#[cfg(windows)]
fn windows_executable_extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}
