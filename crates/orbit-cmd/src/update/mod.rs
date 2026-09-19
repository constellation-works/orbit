//! `orbit update` — install a published Orbit release and converge to it.
//!
//! The command is one linear pipeline with an explicit, idempotent order:
//! decide the target version, refuse a channel Orbit does not own, take the
//! update lock, re-read the installed version (resolving a replaced Linux
//! inode back to the live path), then stage and authenticate the archive,
//! swap it in atomically, confirm the new executable reports the version
//! that was asked for, then let *that* executable migrate `.orbit/` state,
//! reconcile managed assets, and repoint the host clock unit.
//!
//! Migration runs before managed-asset sync because a layout migration can
//! move the directories those assets live in; converging assets first would
//! write them into the shape the upgrade is about to leave behind. The clock
//! unit is converged last because it is host state rather than workspace
//! state: it is the one step that still runs outside a workspace, and
//! re-arming it against a workspace whose migration failed would only produce
//! a failing sweep every minute.
//!
//! Every stage before the swap fails with nothing changed. After the swap the
//! rule inverts: `.orbit/` may already be partly migrated, so recovery is
//! forward — re-running `orbit update` re-enters at the convergence steps,
//! which are the same idempotent operations the operator would run by hand.

pub mod channel;
pub mod converge;
pub mod lock;
pub mod source;
pub mod stage;
pub mod version;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use orbit_common::OrbitError;
use orbit_common::security::release::{TRUSTED_RELEASE_KEYS, TrustedReleaseKey};
use serde::Serialize;

use crate::registry_runtime::RegisteredRuntimeFactory;
use channel::InstallChannel;
use converge::{ConvergenceStep, run_reporting_step, run_step};
use lock::UpdateLock;
use source::{ReleaseSource, release_source_from_env};
use stage::{restore_backup, stage_release};
use version::ReleaseVersion;

/// Exit code for an update that installed the new executable but could not
/// finish converging workspace state. Distinct from a plain failure: the
/// installation moved, and the operator has to finish it.
pub const EXIT_NEEDS_RECOVERY: i32 = 4;

/// Exit code for `--check` when a newer release is available.
pub const EXIT_UPDATE_AVAILABLE: i32 = 3;

/// What the operator asked `orbit update` to do.
#[derive(Debug, Clone, Default)]
pub struct UpdateRequest {
    /// Install this exact version instead of the newest published one.
    pub target_version: Option<String>,
    /// Report only; download nothing and change nothing.
    pub check: bool,
    /// Permit installing a release older than the running one.
    pub allow_downgrade: bool,
}

/// The machine facts an update runs against.
///
/// Constructed from the process by [`UpdateEnvironment::from_process`]; tests
/// build one directly so the whole flow runs against fixtures without touching
/// a real installation.
pub struct UpdateEnvironment {
    /// Authoritative host root for cross-process upgrade admission.
    pub global_root: PathBuf,
    /// The executable to replace.
    pub executable: PathBuf,
    /// Process snapshot of the running binary's version (`CARGO_PKG_VERSION`
    /// in production). `--check` uses this; mutation uses the on-disk
    /// version re-read under the install lock.
    pub current_version: String,
    /// Release target triple for this platform.
    pub target_triple: String,
    /// Which installer owns [`Self::executable`].
    pub install_channel: InstallChannel,
    /// Where release artifacts are read from.
    pub source: Box<dyn ReleaseSource>,
    /// Release signing keys to accept.
    pub trusted_keys: &'static [TrustedReleaseKey],
    /// Today's date, for signing-key expiry.
    pub today: NaiveDate,
    /// The initialized Orbit workspace selected for convergence, if any.
    pub workspace: Option<UpdateWorkspace>,
}

/// The process location and resolved root that update subprocesses must retain.
pub struct UpdateWorkspace {
    /// Working directory inherited by the replacement executable.
    pub cwd: PathBuf,
    /// Authoritative `.orbit` root selected by `--root`, `ORBIT_ROOT`, or cwd discovery.
    pub root: PathBuf,
    /// Root argument to forward when selection was explicit rather than cwd-based.
    pub root_argument: Option<PathBuf>,
}

impl UpdateEnvironment {
    /// Read this process's own installation, platform, and workspace.
    pub fn from_process(root_override: Option<&Path>) -> Result<Self, OrbitError> {
        let executable =
            converge::resolve_installed_executable(&std::env::current_exe().map_err(|error| {
                OrbitError::Io(format!("cannot locate the running orbit: {error}"))
            })?);
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        let root_was_explicit = root_override.is_some()
            || std::env::var("ORBIT_ROOT").is_ok_and(|root| !root.trim().is_empty());
        let workspace =
            RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?.map(
                |roots| UpdateWorkspace {
                    cwd,
                    root_argument: root_was_explicit.then(|| roots.shared_root.clone()),
                    root: roots.shared_root,
                },
            );
        Ok(Self {
            global_root: orbit_core::runtime::resolve_global_root()?,
            install_channel: InstallChannel::detect_with_homebrew_ownership(
                &executable,
                channel::managed_install_dir().as_deref(),
                &channel::SystemHomebrewInventory::system(),
            ),
            executable,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            target_triple: channel::release_target_triple()?.to_string(),
            source: release_source_from_env(),
            trusted_keys: TRUSTED_RELEASE_KEYS,
            today: chrono::Utc::now().date_naive(),
            workspace,
        })
    }
}

/// How an update run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    /// The requested version is already installed and state is converged.
    AlreadyCurrent,
    /// `--check` found a different version to install.
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
    /// What the operator must do to finish, when the run did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
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

/// Run one update.
pub fn run_update(
    environment: &UpdateEnvironment,
    request: &UpdateRequest,
) -> Result<UpdateReport, OrbitError> {
    let snapshot = ReleaseVersion::parse(&environment.current_version)?;
    let target = match &request.target_version {
        Some(requested) => ReleaseVersion::parse(requested)?,
        None => ReleaseVersion::parse(&environment.source.latest_version()?)?,
    };
    let asset = channel::release_archive_name(&environment.target_triple);
    let remediation = environment
        .install_channel
        .unsupported_reason(&environment.executable, &target.to_string());
    let mut report = UpdateReport {
        install_channel: environment.install_channel.as_str(),
        updatable: remediation.is_none(),
        remediation,
        executable: environment.executable.clone(),
        release_source: environment.source.describe(),
        target: environment.target_triple.clone(),
        asset: asset.clone(),
        current_version: snapshot.to_string(),
        target_version: target.to_string(),
        outcome: UpdateOutcome::AlreadyCurrent,
        replaced: false,
        archive_sha256: None,
        signing_key_id: None,
        backup_path: None,
        steps: Vec::new(),
        workspace_root: environment
            .workspace
            .as_ref()
            .map(|workspace| workspace.root.clone()),
        recovery: None,
    };

    if request.check {
        // Read-only: report what an update would do, including for a channel
        // Orbit does not own. Refusing here would make "is there an update?"
        // fail for a reason that has nothing to do with the answer.
        report.outcome = if target == snapshot {
            UpdateOutcome::AlreadyCurrent
        } else {
            UpdateOutcome::UpdateAvailable
        };
        return Ok(report);
    }

    // Everything past this point can write, so the channel guard comes first.
    if let Some(remediation) = environment
        .install_channel
        .unsupported_reason(&environment.executable, &target.to_string())
    {
        return Err(OrbitError::InvalidInput(remediation));
    }

    let admission =
        orbit_common::fs::generation::GenerationUpdate::acquire(&environment.global_root)?;
    let install_dir = environment.executable.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "'{}' has no parent directory",
            environment.executable.display()
        ))
    })?;
    let _lock = UpdateLock::acquire(install_dir)?;

    // Another update may have finished between the process snapshot and this
    // lock. Version decisions follow the live installed path, not the running
    // inode or the compile-time snapshot.
    let executable = converge::resolve_installed_executable(&environment.executable);
    let current = locked_installed_version(&executable)?;
    report.executable = executable.clone();
    report.current_version = current.to_string();
    if current != snapshot {
        tracing::info!(
            snapshot = %snapshot,
            installed = %current,
            executable = %executable.display(),
            "installed orbit version changed before the update lock was acquired"
        );
    }

    if target < current && !request.allow_downgrade {
        return Err(OrbitError::InvalidInput(format!(
            "refusing to replace orbit {current} with the older release {target}; \
             an older binary cannot open workspace state a newer one has already migrated. \
             Pass --allow-downgrade to attempt it anyway (it is checked against this \
             workspace before anything is replaced)"
        )));
    }

    if target == current {
        // Not a no-op: re-running `orbit update` at the installed version is
        // the documented way to finish a run whose convergence failed.
        converge::require_admission_contract(&executable)?;
        let digest = orbit_common::fs::generation::executable_generation(&executable)?;
        let _generation = admission.pin(&digest)?;
        return Ok(finish(
            environment,
            &executable,
            report,
            UpdateOutcome::AlreadyCurrent,
        ));
    }

    let staged = stage_release(
        environment.source.as_ref(),
        &target.to_string(),
        &asset,
        &executable,
        environment.trusted_keys,
        environment.today,
    )?;
    report.archive_sha256 = Some(staged.archive_sha256.clone());
    report.signing_key_id = Some(staged.signing_key_id.clone());

    converge::require_admission_contract(staged.path())?;
    if target < current {
        assert_downgrade_is_compatible(environment, staged.path(), &current, &target)?;
    }

    let digest = orbit_common::fs::generation::executable_generation(staged.path())?;
    let backup = backup_path(&executable);
    staged.commit(&executable, &backup)?;
    report.replaced = true;
    report.backup_path = Some(backup.clone());

    // Nothing has touched `.orbit/` yet, so a binary that does not identify
    // itself as the requested version is still safely reversible.
    let installed =
        converge::probe_version(&executable).and_then(|reported| ReleaseVersion::parse(&reported));
    match installed {
        Ok(installed) if installed == target => {}
        Ok(installed) => {
            restore_backup(&executable, &backup)?;
            return Err(OrbitError::Execution(format!(
                "the release published as {target} reports itself as {installed}; \
                 restored the previous executable and changed no workspace state"
            )));
        }
        Err(error) => {
            restore_backup(&executable, &backup)?;
            return Err(OrbitError::Execution(format!(
                "the installed release could not be verified ({error}); \
                 restored the previous executable and changed no workspace state"
            )));
        }
    }

    let _generation = match admission.pin(&digest) {
        Ok(guard) => guard,
        Err(error) => {
            report.outcome = UpdateOutcome::NeedsRecovery;
            report.recovery = Some(format!(
                "The executable was replaced but candidate admission failed: {error}. No convergence was attempted."
            ));
            return Ok(report);
        }
    };
    Ok(finish(
        environment,
        &executable,
        report,
        UpdateOutcome::Updated,
    ))
}

/// Probe the on-disk executable after the install lock is held.
fn locked_installed_version(executable: &Path) -> Result<ReleaseVersion, OrbitError> {
    let reported = converge::probe_version(executable)?;
    ReleaseVersion::parse(&reported)
}

/// Run the convergence steps and settle the outcome.
fn finish(
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

/// Migrate `.orbit/` state, reconcile managed assets, then repoint the host
/// clock unit — in that order.
fn converge_workspace(environment: &UpdateEnvironment, executable: &Path) -> Vec<ConvergenceStep> {
    let mut steps = workspace_steps(environment, executable);
    steps.push(clock_step(environment, executable, &steps));
    steps
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
    let command = |args: &str| {
        root_argument.map_or_else(
            || format!("`orbit {args}`"),
            |root| format!("`orbit --root {} {args}`", root.display()),
        )
    };
    let retry = command("update");
    let direct = failed
        .iter()
        .map(|args| command(args))
        .collect::<Vec<_>>()
        .join(" and ");
    let mut text = format!(
        "orbit {} is installed, but {direct} did not finish. \
         Re-run {retry} from this workspace to retry — every step is idempotent — \
         or run {direct} directly and read its diagnostics.",
        report.target_version,
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

/// Refuse a downgrade the target release cannot actually open.
///
/// Asks the *staged* binary — before it is installed — whether it can read
/// this workspace for writes. A zero exit code also permits additive-newer
/// read-only inspections, so require the structured current/supported versions
/// and explicit up-to-date result instead of treating success as compatibility.
fn assert_downgrade_is_compatible(
    environment: &UpdateEnvironment,
    staged: &Path,
    current: &ReleaseVersion,
    target: &ReleaseVersion,
) -> Result<(), OrbitError> {
    let Some(workspace) = environment.workspace.as_ref() else {
        return Ok(());
    };
    if converge::probe_writable_state(staged, &workspace.cwd, workspace.root_argument.as_deref())? {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "orbit {target} cannot open this workspace's state for audited writes, which orbit {current} \
         has already migrated, or did not provide a supported compatibility report; nothing was \
         replaced. Restore a compatible backup before downgrading. Read-only inspection success \
         is insufficient for an MCP authority"
    )))
}

/// Where the outgoing executable is preserved.
fn backup_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_os_string();
    name.push(".previous");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests;
