//! The update report, its outcome, and the post-replacement convergence that
//! settles it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::fs::generation::{ParticipantRecord, ParticipantRole};
use serde::Serialize;

use super::converge::{ConvergenceStep, run_reporting_step, run_step};
use super::environment::UpdateEnvironment;
use super::local_candidate::LocalCandidateEvidence;
use super::version::ReleaseVersion;

/// Exit code for an update that installed the new executable but could not
/// finish converging workspace state. Distinct from a plain failure: the
/// installation moved, and the operator has to finish it.
pub const EXIT_NEEDS_RECOVERY: i32 = 4;

/// Exit code for `--check` when a newer release is available.
pub const EXIT_UPDATE_AVAILABLE: i32 = 3;

/// How an update run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    /// `--check` found no newer release, or the requested version is already
    /// installed and state is converged after applying an update.
    AlreadyCurrent,
    /// `--check` found a newer version to install.
    UpdateAvailable,
    /// The executable and workspace state are both at the target version.
    Updated,
    /// The executable is at the target version but convergence did not finish.
    NeedsRecovery,
}

/// The result of an update run, rendered as the command's payload.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateReport {
    /// Which installer owns the executable.
    pub install_channel: &'static str,
    /// Whether `orbit update` may replace this executable in place.
    pub updatable: bool,
    /// The command to run instead, when it may not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// The executable considered for replacement.
    pub executable: PathBuf,
    /// Where release artifacts were read from.
    pub release_source: String,
    /// Release target triple.
    pub target: String,
    /// Release archive name for this platform.
    pub asset: String,
    /// Version before the run.
    pub current_version: String,
    /// Version the run selected.
    pub target_version: String,
    /// How the run ended.
    pub outcome: UpdateOutcome,
    /// Whether the executable on disk was actually replaced.
    pub replaced: bool,
    /// SHA-256 of the verified release archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    /// Release signing key that authenticated the checksum manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
    /// Where the outgoing executable was preserved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
    /// Post-replacement convergence steps, in the order they ran.
    pub steps: Vec<ConvergenceStep>,
    /// Workspace root selected for convergence, when one was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
    /// Generation authorities admission was taken on, in lock order.
    pub admission_roots: Vec<PathBuf>,
    /// Provenance of an operator-built candidate; absent for a release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_candidate: Option<LocalCandidateEvidence>,
    /// Live processes admitted beside the update because they hand over to
    /// the candidate once it is renamed over the executable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub handover: Vec<HandoverProcess>,
    /// What the operator must do to finish, when the run did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
}

/// A live process that hands over to the installed candidate.
#[derive(Debug, Clone, Serialize)]
pub struct HandoverProcess {
    /// Its process id, which the handover keeps.
    pub pid: u32,
    /// The role it registered.
    pub role: ParticipantRole,
    /// When it joined the authority.
    pub started_at: DateTime<Utc>,
    /// The resume capability it hands over with.
    pub resume: Option<String>,
}

impl From<&ParticipantRecord> for HandoverProcess {
    fn from(record: &ParticipantRecord) -> Self {
        Self {
            pid: record.pid,
            role: record.role,
            started_at: record.started_at,
            resume: record.handover.clone(),
        }
    }
}

impl std::fmt::Display for HandoverProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pid {} ({}, started {})",
            self.pid,
            self.role,
            self.started_at.to_rfc3339_opts(SecondsFormat::Secs, true)
        )
    }
}

impl UpdateReport {
    /// Process exit code for this outcome.
    pub fn exit_code(&self) -> i32 {
        match self.outcome {
            UpdateOutcome::AlreadyCurrent | UpdateOutcome::Updated => 0,
            UpdateOutcome::UpdateAvailable => EXIT_UPDATE_AVAILABLE,
            UpdateOutcome::NeedsRecovery => EXIT_NEEDS_RECOVERY,
        }
    }
}

/// Run the convergence steps and settle the outcome.
pub(super) fn finish(
    environment: &UpdateEnvironment,
    executable: &Path,
    mut report: UpdateReport,
    success_outcome: UpdateOutcome,
) -> UpdateReport {
    report.steps = converge_workspace(environment, executable);
    let failed: Vec<&str> = report
        .steps
        .iter()
        .filter(|step| step.failed())
        .map(|step| step.command.as_str())
        .collect();
    if failed.is_empty() {
        report.outcome = success_outcome;
        return report;
    }
    report.outcome = UpdateOutcome::NeedsRecovery;
    report.recovery = Some(recovery_text(
        &report,
        &failed,
        environment
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.root_argument.as_deref()),
    ));
    report
}

/// The host-wide step that keeps the OS clock unit naming the installed
/// binary. Runs last, and as the replacement executable, so the unit it writes
/// names the binary this update just installed.
const CLOCK_STEP: &[&str] = &["clock", "repair"];

/// Host preparation run by the replacement executable, which re-installs the
/// bundled Bubblewrap when it is older than the one the new release pins.
const BUNDLED_BWRAP_STEP: &[&str] = &["init", "--host-prerequisites-only", "--non-interactive"];

/// Migrate `.orbit/` state, reconcile managed assets, then repoint the host
/// clock unit — in that order — and refresh the bundled Bubblewrap when this
/// host uses one.
fn converge_workspace(environment: &UpdateEnvironment, executable: &Path) -> Vec<ConvergenceStep> {
    let mut steps = workspace_steps(environment, executable);
    steps.push(clock_step(environment, executable, &steps));
    steps.extend(bundled_bwrap_step(environment, executable));
    steps
}

/// The bundled Bubblewrap is part of the Orbit release, so it moves with
/// Orbit. Only a host that already has one installed runs this step: the
/// distribution's own Bubblewrap is not Orbit's to change during an update.
/// Like the clock step it is host-wide, and like `orbit init` it never
/// prompts — a host without passwordless sudo reports the step as failed with
/// the command to run by hand.
fn bundled_bwrap_step(
    environment: &UpdateEnvironment,
    executable: &Path,
) -> Option<ConvergenceStep> {
    if !environment.bundled_bwrap_installed {
        return None;
    }
    let cwd = match environment.workspace.as_ref() {
        Some(workspace) => workspace.cwd.clone(),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                return Some(ConvergenceStep::skipped(
                    &BUNDLED_BWRAP_STEP.join(" "),
                    &format!("could not resolve the current directory: {error}"),
                ));
            }
        },
    };
    // Host preparation refuses `--root`: it touches no workspace.
    Some(run_step(executable, &cwd, None, BUNDLED_BWRAP_STEP))
}

fn workspace_steps(environment: &UpdateEnvironment, executable: &Path) -> Vec<ConvergenceStep> {
    const STEPS: [&[&str]; 2] = [&["migrate", "--confirm"], &["workspace", "sync"]];
    let Some(workspace) = environment.workspace.as_ref() else {
        return STEPS
            .iter()
            .map(|args| {
                ConvergenceStep::skipped(
                    &args.join(" "),
                    "the current directory is not an initialized Orbit workspace; \
                     run this from each workspace after upgrading",
                )
            })
            .collect();
    };
    let mut steps = Vec::new();
    for args in STEPS {
        let step = run_step(
            executable,
            &workspace.cwd,
            workspace.root_argument.as_deref(),
            args,
        );
        let stop = step.failed();
        steps.push(step);
        if stop {
            // A failed migration leaves managed-asset paths undefined, so
            // reconciling them next would write into a shape that is still
            // mid-upgrade.
            break;
        }
    }
    steps
}

/// Repoint the installed clock unit at the replacement executable.
///
/// The unit embeds an absolute program path, so an install at a new location —
/// Homebrew to `~/.orbit/bin`, say — leaves launchd or systemd invoking a
/// binary that may no longer exist. Nothing else rewrites it, and a unit whose
/// program is gone stops sweeping silently, so convergence owns it. The step is
/// host-wide: it runs whether or not this directory is an Orbit workspace.
fn clock_step(
    environment: &UpdateEnvironment,
    executable: &Path,
    earlier: &[ConvergenceStep],
) -> ConvergenceStep {
    let command = CLOCK_STEP.join(" ");
    if earlier.iter().any(ConvergenceStep::failed) {
        return ConvergenceStep::skipped(
            &command,
            "an earlier convergence step failed; re-run `orbit update` once it succeeds",
        );
    }
    let workspace = environment.workspace.as_ref();
    let cwd = match workspace.map(|workspace| workspace.cwd.clone()) {
        Some(cwd) => cwd,
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                return ConvergenceStep::skipped(
                    &command,
                    &format!("could not resolve the current directory: {error}"),
                );
            }
        },
    };
    run_reporting_step(
        executable,
        &cwd,
        workspace.and_then(|workspace| workspace.root_argument.as_deref()),
        CLOCK_STEP,
    )
}

fn recovery_text(report: &UpdateReport, failed: &[&str], root_argument: Option<&Path>) -> String {
    let bundled_bwrap_step = BUNDLED_BWRAP_STEP.join(" ");
    let command = |args: &str| {
        if args == bundled_bwrap_step {
            // Host preparation takes no root, and by hand it may prompt for sudo.
            return "`orbit init --host-prerequisites-only`".to_string();
        }
        root_argument.map_or_else(
            || format!("`orbit {args}`"),
            |root| format!("`orbit --root {} {args}`", root.display()),
        )
    };
    // A local candidate is retried with its own exact invocation: the plain
    // command would resolve a published release instead.
    let (installed, retry) = match &report.local_candidate {
        Some(local) => (
            format!(
                "the local candidate {} (orbit {})",
                local.executable_sha256.value, report.target_version
            ),
            format!("`{}`", local.retry_command),
        ),
        None => {
            let downgrade = match (
                ReleaseVersion::parse(&report.target_version),
                ReleaseVersion::parse(&report.current_version),
            ) {
                (Ok(target), Ok(current)) => target < current,
                _ => false,
            };
            let update_args = if downgrade {
                format!(
                    "update --version {} --allow-downgrade",
                    report.target_version
                )
            } else {
                format!("update --version {}", report.target_version)
            };
            (
                format!("orbit {}", report.target_version),
                command(&update_args),
            )
        }
    };
    let direct = failed
        .iter()
        .map(|args| command(args))
        .collect::<Vec<_>>()
        .join(" and ");
    let mut text = format!(
        "{installed} is installed, but {direct} did not finish. \
         Re-run {retry} from this workspace to retry — every step is idempotent — \
         or run {direct} directly and read its diagnostics."
    );
    if let Some(backup) = &report.backup_path {
        text.push_str(&format!(
            " The previous executable is preserved at {}. Restore it only if `.orbit/` state \
             was not migrated; once a migration has been applied, an older binary refuses to \
             open the workspace by design.",
            backup.display()
        ));
    }
    text
}
