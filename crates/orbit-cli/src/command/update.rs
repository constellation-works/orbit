use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::Args;
use orbit_cmd::update::{
    CandidateManifestRequest, LocalCandidateRequest, UpdateEnvironment, UpdateOutcome,
    UpdateReport, UpdateRequest, run_local_candidate_update, run_update, write_candidate_manifest,
};
use orbit_common::fs::generation;
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
        migrations, reconciles managed workspace assets, then repoints the host scheduler clock\n\
        unit at the installed binary, in that order. Re-running `orbit update` is idempotent and\n\
        is the supported way to finish a run that did not.\n\n\
        Only installations made by Orbit's own installer can be replaced in place. Where a\n\
        package manager owns the binary, orbit reports the command that upgrades it instead.\n\n\
        --local-candidate installs an executable built from source instead of a release. It is\n\
        identified by its SHA-256 and an operator-attested full source commit, never treated as\n\
        a signed release, and replaces an installation at an equal version when its bytes\n\
        differ. Describe the build once with --write-candidate-manifest, then run the update\n\
        through the candidate itself with an explicit --install-target, so an installed binary\n\
        that predates this option is still upgraded through generation admission. Re-running\n\
        the same candidate converges a run that did not finish."
)]
pub struct UpdateCommand {
    /// Install this exact release (for example 0.19.0) instead of the newest published one
    #[arg(long, value_name = "VERSION", conflicts_with = "local_candidate")]
    pub version: Option<String>,
    /// Report the available release without downloading or replacing anything
    #[arg(long, conflicts_with = "local_candidate")]
    pub check: bool,
    /// Permit installing a release older than the running one, if it can still
    /// open this workspace's state
    #[arg(long)]
    pub allow_downgrade: bool,
    /// Describe the executable admission protocol without opening state
    #[arg(long, conflicts_with_all = ["check", "version", "allow_downgrade", "preflight"])]
    pub contract: bool,
    /// Check whether running Orbit processes prevent an upgrade, without opening stores
    #[arg(long, conflicts_with_all = ["check", "version", "allow_downgrade", "local_candidate"])]
    pub preflight: bool,
    /// Install this locally built executable instead of a published release
    /// (operator-attested, never a signed release)
    #[arg(long, value_name = "PATH", requires = "source_commit")]
    pub local_candidate: Option<PathBuf>,
    /// The full Git commit the local candidate was built from (40 or 64 hex digits)
    #[arg(long, value_name = "SHA", requires = "local_candidate")]
    pub source_commit: Option<String>,
    /// The local candidate's manifest, as written by --write-candidate-manifest
    #[arg(
        long,
        value_name = "PATH",
        requires = "local_candidate",
        requires = "install_target"
    )]
    pub candidate_manifest: Option<PathBuf>,
    /// The managed orbit executable a local candidate replaces; never inferred
    #[arg(
        long,
        value_name = "PATH",
        requires = "local_candidate",
        requires = "candidate_manifest"
    )]
    pub install_target: Option<PathBuf>,
    /// Describe the local candidate in a new manifest instead of installing it
    #[arg(
        long,
        value_name = "PATH",
        requires = "local_candidate",
        conflicts_with_all = ["candidate_manifest", "install_target", "allow_downgrade"]
    )]
    pub write_candidate_manifest: Option<PathBuf>,
    /// Emit machine-readable JSON instead of the report.
    #[arg(long)]
    pub json: bool,
}

impl UpdateCommand {
    /// Run the update and render its report.
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        if self.contract {
            let identity = orbit_core::composition::compiled_compatibility();
            return Ok(Payload::detail(
                serde_json::json!({
                    "schema_version": 1,
                    // Updaters that only speak executable-generation-v1 read
                    // this field; every v2 binary still honours that protocol.
                    "contract": generation::LEGACY_GENERATION_CONTRACT,
                    "contracts": [
                        generation::LEGACY_GENERATION_CONTRACT,
                        generation::GENERATION_CONTRACT
                    ],
                    "admission_contract": generation::GENERATION_CONTRACT,
                    "compatibility": identity,
                    "resume": generation::RESUME_CAPABILITIES,
                }),
                format!(
                    "{} (also honours {})\n  compatibility: {identity}\n  resume: {}",
                    generation::GENERATION_CONTRACT,
                    generation::LEGACY_GENERATION_CONTRACT,
                    generation::RESUME_CAPABILITIES.join(", ")
                ),
            )
            .into());
        }
        if self.preflight {
            // Same `admission_authorities` `UpdateEnvironment::from_process`
            // uses for exclusive admission, so a green preflight names every
            // file the following `orbit update` will lock — including the
            // host-global root a `--root`/`ORBIT_ROOT` override does not move
            // the replaced executable out of.
            let roots = orbit_cmd::update::admission_authorities(root_override)?;
            let _admissions = orbit_cmd::update::acquire_admissions(&roots)?;
            let quiesce = generation::quiesce_bound().as_secs();
            return Ok(Payload::detail(
                serde_json::json!({
                    "schema_version": 1,
                    "admitted": true,
                    "reservation": false,
                    "global_root": roots.first(),
                    "admission_roots": roots,
                    "contract": generation::LEGACY_GENERATION_CONTRACT,
                    "admission_contract": generation::GENERATION_CONTRACT,
                    "compatibility": orbit_core::composition::compiled_compatibility(),
                    "quiesce_timeout_secs": quiesce,
                }),
                format!(
                    "Upgrade admission available on {}. This observation does not reserve admission; use orbit update for guarded replacement. Admission follows {}: builds with compatible state versions run side by side, and a breaking migration waits up to {quiesce}s for live Orbit processes to yield.",
                    describe_authorities(&roots),
                    generation::GENERATION_CONTRACT,
                ),
            ).into());
        }
        if let Some(candidate) = self.local_candidate {
            return local_candidate(
                root_override,
                candidate,
                self.source_commit.unwrap_or_default(),
                self.write_candidate_manifest,
                self.candidate_manifest.zip(self.install_target),
                self.allow_downgrade,
            );
        }
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
        Ok(Payload::detail(doc, format_report(&report, self.check))
            .with_exit_code(exit_code)
            .into())
    }
}

/// `--local-candidate`: write its manifest, or install it over the named target.
fn local_candidate(
    root_override: Option<&Path>,
    candidate: PathBuf,
    source_commit: String,
    write_manifest: Option<PathBuf>,
    install: Option<(PathBuf, PathBuf)>,
    allow_downgrade: bool,
) -> CommandOut {
    if let Some(output) = write_manifest {
        let manifest = write_candidate_manifest(&CandidateManifestRequest {
            candidate: candidate.clone(),
            source_commit,
            output: output.clone(),
        })?;
        let text = format!(
            "Wrote {} for {}\n  source:  {} ({})\n  target:  {}\n  sha256:  {}\n",
            output.display(),
            candidate.display(),
            manifest.source_commit,
            manifest.trust,
            manifest.target,
            manifest.executable_sha256,
        );
        let doc = serde_json::json!({
            "manifest_path": output,
            "candidate": candidate,
            "manifest": manifest,
        });
        return Ok(Payload::detail(doc, text).into());
    }
    let Some((manifest, install_target)) = install else {
        return Err(OrbitError::InvalidInput(
            "--local-candidate needs --write-candidate-manifest <PATH> to describe the build, or \
             --candidate-manifest <PATH> and --install-target <PATH> to install it"
                .to_string(),
        ));
    };
    let environment = UpdateEnvironment::for_install_target(root_override, &install_target)?;
    let report = run_local_candidate_update(
        &environment,
        &LocalCandidateRequest {
            candidate,
            manifest,
            source_commit,
            allow_downgrade,
        },
    )?;
    let doc = serde_json::to_value(&report)
        .map_err(|error| OrbitError::Execution(format!("serialize update report: {error}")))?;
    let exit_code = report.exit_code();
    Ok(Payload::detail(doc, format_report(&report, false))
        .with_exit_code(exit_code)
        .into())
}

/// Name every authority the probe locked, in the order `orbit update` takes them.
fn describe_authorities(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(" and ")
}

fn format_report(report: &UpdateReport, check: bool) -> String {
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
    if let Some(local) = &report.local_candidate {
        let _ = writeln!(
            text,
            "  source:   {} ({}, not a signed release)\n  sha256:   {} (computed from the accepted bytes)",
            local.source_commit.value, local.trust, local.executable_sha256.value,
        );
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
        UpdateOutcome::AlreadyCurrent if check => {
            text.push_str("No newer release is available.\n");
        }
        UpdateOutcome::AlreadyCurrent if report.local_candidate.is_some() => {
            text.push_str(
                "The installed executable already is the accepted local candidate; workspace \
                 state is converged.\n",
            );
        }
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
