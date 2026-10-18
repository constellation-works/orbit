//! `orbit browser-kit` — the headless browser kit workers check UIs with.
//!
//! Each subcommand calls `orbit_cmd::browser_kit` and opens no workspace
//! runtime: the kit belongs to the host user, not to a repository.

use clap::{Args, Subcommand};
use orbit_cmd::browser_kit::{
    self, InstallOptions, InstallReport, KitProblem, KitStatus, VerifyReport,
};
use orbit_core::OrbitError;
use orbit_core::bootstrap::browser_kit::{KitLayout, PLAYWRIGHT_VERSION};
use serde_json::json;

use crate::command::{CommandOut, Payload, require_confirmation};

#[derive(Args)]
#[command(
    about = "Install, verify and remove the headless browser kit workers check UIs with",
    long_about = "Install, verify and remove the headless browser kit workers check UIs with\n\n\
        The kit lives at ~/.local/chromium-deps: Playwright at the version this Orbit release \
        pins, and the Chromium headless shell it launches, downloaded from Playwright's CDN. \
        Run it on the host as the user who runs workers. Once `verify` passes, every agent \
        receives PLAYWRIGHT_BROWSERS_PATH, PLAYWRIGHT_MODULE and ORBIT_BROWSER_KIT_ENV (the \
        kit's env.sh, which a check sources first). Node must already be installed."
)]
pub struct BrowserKitCommand {
    #[command(subcommand)]
    pub command: BrowserKitSubcommand,
}

#[derive(Subcommand)]
pub enum BrowserKitSubcommand {
    /// Report the kit's version, browser, libraries and what workers receive
    Status(BrowserKitStatusArgs),
    /// Install, upgrade or adopt the kit at the pinned Playwright version, then verify it
    Install(BrowserKitInstallArgs),
    /// Run the loopback screenshot smoke test and record a pass for workers
    Verify(BrowserKitVerifyArgs),
    /// Delete the kit; workers stop receiving its variables
    Remove(BrowserKitRemoveArgs),
}

#[derive(Args)]
pub struct BrowserKitStatusArgs {}

#[derive(Args)]
#[command(
    after_help = "On Linux, missing Chromium libraries are unpacked into the kit without root \
        on Debian and Ubuntu. --with-deps installs them system-wide with sudo instead. On other \
        distributions the unresolved libraries and the fix are reported.\n\n\
        An existing kit is adopted: steps it already satisfies are skipped, and an env.sh you \
        wrote yourself is kept."
)]
pub struct BrowserKitInstallArgs {
    /// Linux: install Chromium's system libraries with sudo (`playwright install-deps chromium`)
    #[arg(long)]
    pub with_deps: bool,
}

#[derive(Args)]
pub struct BrowserKitVerifyArgs {}

#[derive(Args)]
pub struct BrowserKitRemoveArgs {
    /// Confirm deleting ~/.local/chromium-deps
    #[arg(long)]
    pub confirm: bool,
}

impl BrowserKitCommand {
    pub fn execute_without_runtime(self) -> CommandOut {
        let layout = KitLayout::current_user()?;
        match self.command {
            BrowserKitSubcommand::Status(_) => {
                let status = browser_kit::kit_status(&layout);
                Ok(Payload::detail(status_json(&status)?, status_text(&status)).into())
            }
            BrowserKitSubcommand::Install(args) => {
                let report = browser_kit::install(
                    &layout,
                    InstallOptions {
                        with_deps: args.with_deps,
                    },
                    &mut |step| eprintln!("{step}..."),
                )?;
                let exit_code = i32::from(!report.ready());
                Ok(Payload::detail(to_json(&report)?, install_text(&report))
                    .with_exit_code(exit_code)
                    .into())
            }
            BrowserKitSubcommand::Verify(_) => {
                let report = browser_kit::verify(&layout)?;
                let exit_code = i32::from(!report.passed);
                Ok(Payload::detail(to_json(&report)?, verify_text(&report))
                    .with_exit_code(exit_code)
                    .into())
            }
            BrowserKitSubcommand::Remove(args) => {
                require_confirmation(args.confirm, "removing the browser kit")?;
                let removed = browser_kit::remove(&layout)?;
                let root = layout.root().display().to_string();
                let text = if removed {
                    format!("Removed the browser kit at {root}; workers no longer receive it.")
                } else {
                    format!("No browser kit at {root}.")
                };
                Ok(Payload::detail(json!({ "root": root, "removed": removed }), text).into())
            }
        }
    }
}

fn to_json(value: &impl serde::Serialize) -> Result<serde_json::Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("encode browser kit report: {error}")))
}

/// The status record plus every problem with its fix.
fn status_json(status: &KitStatus) -> Result<serde_json::Value, OrbitError> {
    let mut value = to_json(status)?;
    if let Some(record) = value.as_object_mut() {
        record.insert("problems".to_string(), to_json(&all_problems(status))?);
    }
    Ok(value)
}

fn all_problems(status: &KitStatus) -> Vec<KitProblem> {
    status
        .problems()
        .into_iter()
        .chain(status.library_problem())
        .collect()
}

fn status_text(status: &KitStatus) -> String {
    let kit = &status.inspection;
    let mut lines = vec![format!("browser kit: {}", status.state.replace('_', " "))];
    lines.push(format!("  location:   {}", kit.root.display()));
    if !kit.present {
        lines.push(format!(
            "  next:       orbit browser-kit install (Playwright {PLAYWRIGHT_VERSION})"
        ));
        return lines.join("\n");
    }
    lines.push(format!(
        "  playwright: {} (this release pins {PLAYWRIGHT_VERSION})",
        kit.playwright_version.as_deref().unwrap_or("not installed")
    ));
    if let Some(revision) = &kit.chromium_revision {
        lines.push(format!(
            "  chromium:   chromium_headless_shell-{revision} ({})",
            if kit.browser_installed {
                "installed"
            } else {
                "missing"
            }
        ));
    }
    lines.push(format!(
        "  env.sh:     {}",
        match (kit.env_script_present, kit.env_script_managed) {
            (false, _) => "missing",
            (true, true) => "written by Orbit",
            (true, false) => "written by the operator",
        }
    ));
    lines.push(format!(
        "  node:       {}",
        status.node.as_ref().map_or_else(
            || "not on PATH".to_string(),
            |node| node.display().to_string()
        )
    ));
    if let Some(unresolved) = &status.unresolved_libraries {
        lines.push(format!(
            "  libraries:  {}",
            if unresolved.is_empty() {
                "all resolved".to_string()
            } else {
                format!("unresolved: {}", unresolved.join(", "))
            }
        ));
    }
    lines.push(format!(
        "  verified:   {}",
        match (&kit.verification, status.verified) {
            (Some(verification), true) => verification.verified_at.clone(),
            (Some(_), false) => "no (the kit changed after its last pass)".to_string(),
            (None, _) => "no".to_string(),
        }
    ));
    lines.push(format!(
        "  workers:    {}",
        if status.worker_env.is_empty() {
            "receive nothing".to_string()
        } else {
            format!("receive {}", status.worker_env.join(", "))
        }
    ));
    let problems = all_problems(status);
    if !problems.is_empty() {
        lines.push("problems:".to_string());
        for problem in problems {
            lines.push(format!("  - {}", problem.problem));
            lines.push(format!("    fix: {}", problem.fix));
        }
    }
    lines.join("\n")
}

fn install_text(report: &InstallReport) -> String {
    let mut lines = vec![if report.adopted {
        "Adopted the existing browser kit:".to_string()
    } else {
        "Installed the browser kit:".to_string()
    }];
    lines.extend(report.steps.iter().map(|step| format!("  - {step}")));
    if let Some(libraries) = &report.libraries {
        lines.push(format!("  - libraries: {}", libraries.method));
        if !libraries.unpacked_packages.is_empty() {
            lines.push(format!(
                "    unpacked {} package(s) into the kit",
                libraries.unpacked_packages.len()
            ));
        }
        if let Some(fix) = &libraries.fix {
            lines.push(format!(
                "    unresolved: {}; fix: {fix}",
                libraries.unresolved.join(", ")
            ));
        }
    }
    if let Some(verification) = &report.verification {
        lines.push(verify_text(verification));
    }
    lines.push(String::new());
    lines.push(status_text(&report.status));
    lines.join("\n")
}

fn verify_text(report: &VerifyReport) -> String {
    let mut lines = Vec::new();
    if report.passed {
        lines.push(format!(
            "kit ok: Chromium {} rendered a loopback page{}.",
            report.browser_version.as_deref().unwrap_or("?"),
            report
                .screenshot
                .as_ref()
                .map(|path| format!("; screenshot: {}", path.display()))
                .unwrap_or_default()
        ));
        lines.push("Workers now receive the kit's variables.".to_string());
    } else {
        lines.push(
            "The smoke test failed; workers receive none of the kit's variables.".to_string(),
        );
        if let Some(diagnosis) = &report.diagnosis {
            lines.push(format!("Likely cause: {diagnosis}"));
        }
        if let Some(output) = &report.output {
            lines.push(output.clone());
        }
    }
    lines.push(report.note.to_string());
    lines.join("\n")
}
