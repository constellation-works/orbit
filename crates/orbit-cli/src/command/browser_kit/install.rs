use clap::Args;
use orbit_cmd::browser_kit::{self, InstallOptions, InstallReport};
use orbit_common::Env;
use orbit_core::bootstrap::browser_kit::KitLayout;

use super::support::{status_text, to_json, verify_text};
use crate::command::{CommandOut, Payload};

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

impl BrowserKitInstallArgs {
    pub(super) fn execute(self, layout: &KitLayout, env: &Env) -> CommandOut {
        let report = browser_kit::install(
            layout,
            InstallOptions {
                with_deps: self.with_deps,
            },
            env,
            &mut |step| eprintln!("{step}..."),
        )?;
        let exit_code = i32::from(!report.ready());
        Ok(Payload::detail(to_json(&report)?, install_text(&report))
            .with_exit_code(exit_code)
            .into())
    }
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
