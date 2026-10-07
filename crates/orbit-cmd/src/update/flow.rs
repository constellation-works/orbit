//! The linear update pipeline: decide, lock, stage, verify, admit, swap,
//! verify, pin.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::admission::{
    acquire_admissions, acquire_candidate_admissions, handover_of, pin_candidate,
};
use super::channel;
use super::converge;
use super::environment::UpdateEnvironment;
use super::lock::UpdateLock;
use super::report::{UpdateOutcome, UpdateReport, finish};
use super::stage::{restore_backup, stage_release};
use super::version::ReleaseVersion;

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
        admission_roots: environment.admission_roots.clone(),
        local_candidate: None,
        handover: Vec::new(),
        recovery: None,
    };

    if request.check {
        // Read-only: report whether a newer release exists, including for a channel
        // Orbit does not own. Refusing here would make "is there an update?"
        // fail for a reason that has nothing to do with the answer.
        report.outcome = if target <= snapshot {
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
        // the documented way to finish a run whose convergence failed. It
        // renames nothing, so nothing live can hand over to it.
        let contract = converge::require_admission_contract(&executable)?;
        let digest = orbit_common::fs::generation::executable_generation(&executable)?;
        let admissions = acquire_admissions(&environment.admission_roots)?;
        let _generations = pin_candidate(
            &environment.admission_roots,
            admissions,
            &digest,
            contract.identity.as_ref(),
        )?;
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

    // The signed manifest authenticates the archive, but does not bind it to
    // the requested version. Reject mislabeled releases before any backup or
    // swap, so an unrequested binary is never exposed at the installed path.
    let reported = converge::probe_version(staged.path())
        .and_then(|reported| ReleaseVersion::parse(&reported))
        .map_err(|error| {
            OrbitError::Execution(format!(
                "the staged release could not be verified ({error}); nothing was replaced"
            ))
        })?;
    if reported != target {
        return Err(OrbitError::Execution(format!(
            "the release published as {target} reports itself as {reported}; nothing was replaced"
        )));
    }

    let contract = converge::require_admission_contract(staged.path())?;
    if target < current {
        assert_downgrade_is_compatible(environment, staged.path(), &current, &target)?;
    }

    let digest = orbit_common::fs::generation::executable_generation(staged.path())?;
    // Admission comes only now, once the candidate is staged and verified:
    // knowing what the candidate resumes is what lets a live process that
    // will hand over to it after the rename be admitted beside.
    let admissions =
        acquire_candidate_admissions(&environment.admission_roots, &contract.handover)?;
    report.handover = handover_of(&admissions).iter().map(Into::into).collect();
    let backup = backup_path(&executable);
    staged.commit(&executable, &backup)?;
    report.replaced = true;
    report.backup_path = Some(backup.clone());

    // Verify the installed path as well before touching workspace state.
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

    let _generations = match pin_candidate(
        &environment.admission_roots,
        admissions,
        &digest,
        contract.identity.as_ref(),
    ) {
        Ok(guards) => guards,
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
pub(super) fn locked_installed_version(executable: &Path) -> Result<ReleaseVersion, OrbitError> {
    let reported = converge::probe_version(executable)?;
    ReleaseVersion::parse(&reported)
}

/// Refuse a downgrade the target release or local candidate cannot actually open.
///
/// Asks the *staged* binary — before it is installed — whether it can read
/// this workspace for writes. A zero exit code also permits additive-newer
/// read-only inspections, so require the structured current/supported versions
/// and explicit up-to-date result instead of treating success as compatibility.
pub(super) fn assert_downgrade_is_compatible(
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
pub(super) fn backup_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_os_string();
    name.push(".previous");
    PathBuf::from(name)
}
