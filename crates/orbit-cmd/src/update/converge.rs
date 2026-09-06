//! Bringing workspace state up to the version that was just installed.
//!
//! These steps run as subprocesses of the *replacement* executable, not this
//! one. That is the whole point: only the new binary carries the migration
//! registry and the managed-asset definitions for the version being installed,
//! so asking the outgoing process to converge state would apply the version
//! the operator is leaving.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use serde::Serialize;

/// How much of a failing step's stderr to carry into the report.
const DETAIL_LIMIT: usize = 2000;

/// How long to keep retrying a freshly written executable that reports
/// [`std::io::ErrorKind::ExecutableFileBusy`].
const EXEC_BUSY_WINDOW: Duration = Duration::from_secs(2);

/// What happened to one post-replacement convergence step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Ran and succeeded.
    Succeeded,
    /// Not attempted, with a recorded reason.
    Skipped,
    /// Ran and failed.
    Failed,
}

/// The outcome of one convergence step, as reported to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConvergenceStep {
    /// The `orbit` invocation this step performs, e.g. `migrate --confirm`.
    pub command: String,
    /// Whether it ran, and how it ended.
    pub status: StepStatus,
    /// Process exit code, when it ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Why it was skipped, or what it said when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ConvergenceStep {
    /// Record a step that was deliberately not attempted.
    pub fn skipped(command: &str, reason: &str) -> Self {
        Self {
            command: command.to_string(),
            status: StepStatus::Skipped,
            exit_code: None,
            detail: Some(reason.to_string()),
        }
    }

    /// Whether this step leaves the installation needing operator follow-up.
    pub fn failed(&self) -> bool {
        self.status == StepStatus::Failed
    }
}

/// Run an Orbit convergence command in `cwd`, retaining an explicit root when selected.
pub fn run_step(
    executable: &Path,
    cwd: &Path,
    root_argument: Option<&Path>,
    args: &[&str],
) -> ConvergenceStep {
    let command = args.join(" ");
    let mut process = Command::new(executable);
    if let Some(root) = root_argument {
        process.arg("--root").arg(root);
    }
    let output = run_process(process.args(args).current_dir(cwd));
    match output {
        Ok(output) if output.status.success() => ConvergenceStep {
            command,
            status: StepStatus::Succeeded,
            exit_code: output.status.code(),
            detail: None,
        },
        Ok(output) => ConvergenceStep {
            command,
            status: StepStatus::Failed,
            exit_code: output.status.code(),
            detail: Some(truncate(&String::from_utf8_lossy(&output.stderr))),
        },
        Err(error) => ConvergenceStep {
            command,
            status: StepStatus::Failed,
            exit_code: None,
            detail: Some(format!("failed to run '{}': {error}", executable.display())),
        },
    }
}

/// Path of the installed executable that version checks and replacement use.
///
/// Linux exposes a process whose executable inode was unlinked as
/// `/installed/path (deleted)`. That names the still-running image, not the
/// file now sitting at the install location. After another `orbit update`
/// has replaced the binary, mutation decisions must follow the live path.
/// A real file whose name ends in ` (deleted)` is left alone.
pub fn resolve_installed_executable(executable: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let current_path_is_missing = matches!(
            std::fs::metadata(executable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        if current_path_is_missing
            && let Some(installed_path) = executable
                .as_os_str()
                .as_bytes()
                .strip_suffix(b" (deleted)")
        {
            return PathBuf::from(OsString::from_vec(installed_path.to_vec()));
        }
    }

    executable.to_path_buf()
}

/// Ask `executable` what version it is.
pub fn probe_version(executable: &Path) -> Result<String, OrbitError> {
    let output = run_process(Command::new(executable).arg("--version")).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to run '{} --version': {error}",
            executable.display()
        ))
    })?;
    if !output.status.success() {
        return Err(OrbitError::Execution(format!(
            "'{} --version' failed: {}",
            executable.display(),
            truncate(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    // `clap` renders `<name> <version>`; take the last whitespace-separated
    // field of the first line so a renamed binary still reports usefully.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next_back())
        .map(str::to_string)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "'{} --version' printed nothing parseable",
                executable.display()
            ))
        })
}

/// Run `command`, retrying while the OS reports the executable as busy.
///
/// Linux returns `ETXTBSY` when a binary is exec'd while some process still
/// holds it open for writing. Orbit has just written this file, and any thread
/// that forks in the same window can briefly inherit that descriptor — so a
/// freshly staged or freshly installed executable is retried for a bounded
/// period rather than reported as broken.
fn run_process(command: &mut Command) -> std::io::Result<Output> {
    let deadline = Instant::now() + EXEC_BUSY_WINDOW;
    loop {
        match command.output() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

fn truncate(value: &str) -> String {
    let trimmed = value.trim();
    match trimmed.char_indices().nth(DETAIL_LIMIT) {
        Some((offset, _)) => format!("{}…", &trimmed[..offset]),
        None => trimmed.to_string(),
    }
}
