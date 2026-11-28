use clap::Args;
use orbit_common::Env;
use orbit_core::app::host::KitLayout;
use orbit_core::app::host::browser_kit::{self, InstallOptions, InstallReport};

use super::support::{status_text, to_json, verify_text};
use crate::command::{CommandOut, Payload};

#[derive(Args)]
#[command(
    after_help = "An existing kit is adopted: steps it already satisfies are skipped, and an \
                  env.sh you wrote yourself is kept. On Linux, --with-deps installs Chromium's \
                  libraries system-wide with sudo.\n\n\
                  Examples:\n  \
                  orbit config browser-kit install\n  \
                  orbit config browser-kit install --with-deps",
    after_long_help = "An existing kit is adopted: steps it already satisfies are skipped, and \
                       an env.sh you wrote yourself is kept. On Linux, --with-deps installs \
                       Chromium's libraries system-wide with sudo. Without it, supported \
                       distributions unpack the missing libraries into the kit without root; \
                       other distributions report the unresolved libraries and the fix.\n\n\
                       Library install by distribution: installed orbit-setup skill \
                       `references/browser-checks.md`.\n\n\
                       Examples:\n  \
                       orbit config browser-kit install\n  \
                       orbit config browser-kit install --with-deps"
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
