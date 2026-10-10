use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_core::application::routines::install_clock;
use orbit_registry::load_machine_identity;
use serde_json::json;

#[derive(Args)]
#[command(
    after_help = "Reads the [machine] identity from the selected Orbit root's config.toml\n\
                        (created by `orbit init`) and, with --install-clock, installs the\n\
                        per-user OS clock unit that invokes `orbit clock tick` on the clock\n\
                        cadence, every minute by default (launchd on macOS, a systemd user\n\
                        timer on Linux). It never creates or rewrites machine identity —\n\
                        run `orbit init` for that. `orbit clock` controls the installed unit."
)]
pub struct RoutineInitArgs {
    /// Also install and activate the OS clock unit driving `orbit clock tick`.
    #[arg(long)]
    pub install_clock: bool,
}

impl RoutineInitArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        // Read-only: machine identity is owned by `orbit init`. Fail closed
        // with an actionable error when it is absent.
        let identity = load_machine_identity(global_root)?;
        let mut text = format!(
            "machine identity: name=\"{}\", id={}",
            identity.name, identity.id
        );

        if !self.install_clock {
            text.push_str(
                "\nclock unit not installed (pass --install-clock to set up `orbit clock tick`)",
            );
            return Ok(Payload::detail(
                json!({
                    "machine": { "name": identity.name, "id": identity.id },
                    "clock": { "installed": false },
                }),
                text,
            )
            .into());
        }

        let report = install_clock(global_root)?;
        for file in &report.files_written {
            text.push_str(&format!("\nwrote {}", file.display()));
        }
        text.push_str(&format!(
            "\n{} {} (owner-only; the clock loads the `execution.env.pass` names listed here, e.g. CLAUDE_CODE_OAUTH_TOKEN)",
            if report.env_file_created { "created" } else { "kept" },
            report.env_file.display()
        ));
        if report.activated {
            text.push_str(
                "\nclock unit active: `orbit clock tick` runs on this machine's clock cadence (`orbit clock status`)",
            );
        } else {
            text.push_str("\nclock unit files written but not activated; run:");
            for step in &report.manual_steps {
                text.push_str(&format!("\n  {step}"));
            }
        }
        Ok(Payload::detail(
            json!({
                "machine": { "name": identity.name, "id": identity.id },
                "clock": {
                    "installed": true,
                    "activated": report.activated,
                    "files_written": report
                        .files_written
                        .iter()
                        .map(|file| file.display().to_string())
                        .collect::<Vec<_>>(),
                    "manual_steps": report.manual_steps,
                    "env_file": report.env_file.display().to_string(),
                    "env_file_created": report.env_file_created,
                },
            }),
            text,
        )
        .into())
    }
}
