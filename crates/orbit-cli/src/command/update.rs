use std::fmt::Write as _;
use std::path::Path;

use clap::Args;
use orbit_cmd::update::{
    UpdateEnvironment, UpdateOutcome, UpdateReport, UpdateRequest, run_update,
};
use orbit_core::OrbitError;

use crate::command::{CommandOut, Payload};

// `orbit update` — replace the installed Orbit executable and converge state.
//
// Dispatched without a runtime: it decides which workspace to converge for
// itself, and opening one here would run this binary's migrations rather than
// the ones shipped by the version being installed. Clap renders `///` on a
// command or argument as user-facing help, so that rationale stays in `//`.
#[derive(Args)]
#[command(
    about = "Install the latest published Orbit release, or an explicitly requested version",
    long_about = "Install the latest published Orbit release, or an explicitly requested version.\n\n\
        With no arguments, orbit resolves the newest published release and installs it. Pass\n\
        --version to install one exact release instead. The download is checked against the\n\
        signed release checksum manifest before anything is replaced.\n\n\
        After the executable is replaced, the new binary applies pending .orbit layout and store\n\
        migrations and then reconciles managed workspace assets, in that order. Re-running\n\
        `orbit update` is idempotent and is the supported way to finish a run that did not.\n\n\
        Only installations made by Orbit's own installer can be replaced in place. Where a\n\
        package manager owns the binary, orbit reports the command that upgrades it instead."
)]
pub struct UpdateCommand {
    /// Install this exact release (for example 0.19.0) instead of the newest published one
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Report the available release without downloading or replacing anything
    #[arg(long)]
    pub check: bool,
    /// Permit installing a release older than the running one, if it can still
    /// open this workspace's state
    #[arg(long)]
    pub allow_downgrade: bool,
    /// Emit machine-readable JSON instead of the report.
    #[arg(long)]
    pub json: bool,
}

impl UpdateCommand {
    /// Run the update and render its report.
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let environment = UpdateEnvironment::from_process(root_override)?;
        let report = run_update(
            &environment,
            &UpdateRequest {
                target_version: self.version,
                check: self.check,
                allow_downgrade: self.allow_downgrade,
            },
        )?;
        let doc = serde_json::to_value(&report)
            .map_err(|error| OrbitError::Execution(format!("serialize update report: {error}")))?;
        let exit_code = report.exit_code();
        Ok(Payload::detail(doc, format_report(&report))
            .with_exit_code(exit_code)
            .into())
    }
}

fn format_report(report: &UpdateReport) -> String {
    let mut text = format!(
        "orbit {} → {}\n  install:  {} ({})\n  platform: {}\n  releases: {}\n",
        report.current_version,
        report.target_version,
        report.executable.display(),
        report.install_channel,
        report.target,
        report.release_source,
    );
    if let Some(key_id) = &report.signing_key_id {
        let _ = writeln!(text, "  verified: signed by {key_id}");
    }
    if let Some(checksum) = &report.archive_sha256 {
        let _ = writeln!(text, "  sha256:   {checksum}");
    }
    if let Some(backup) = &report.backup_path {
        let _ = writeln!(text, "  previous: {}", backup.display());
    }
    for step in &report.steps {
        let _ = writeln!(
            text,
            "  {} orbit {}{}",
            match step.status {
                orbit_cmd::update::converge::StepStatus::Succeeded => "ok",
                orbit_cmd::update::converge::StepStatus::Skipped => "--",
                orbit_cmd::update::converge::StepStatus::Failed => "!!",
            },
            step.command,
            step.detail
                .as_deref()
                .map(|detail| format!(" — {detail}"))
                .unwrap_or_default()
        );
    }
    text.push('\n');
    match report.outcome {
        UpdateOutcome::AlreadyCurrent => {
            text.push_str("Already on the requested release; workspace state is converged.\n");
        }
        UpdateOutcome::UpdateAvailable => match &report.remediation {
            Some(remediation) => {
                let _ = writeln!(text, "An update is available, but {remediation}.");
            }
            None => text.push_str("An update is available; run `orbit update` to install it.\n"),
        },
        UpdateOutcome::Updated => {
            text.push_str(
                "Updated. Restart long-lived Orbit services and pipeline workers so newly \
                 dispatched agents inherit the replacement build, and run `orbit update` from \
                 any other workspace that needs converging.\n",
            );
        }
        UpdateOutcome::NeedsRecovery => {
            let _ = writeln!(
                text,
                "{}",
                report
                    .recovery
                    .as_deref()
                    .unwrap_or("The update did not finish; re-run `orbit update`.")
            );
        }
    }
    text
}
