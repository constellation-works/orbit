use std::path::Path;

use crate::command::{CommandOut, CommandOutput};
use clap::Args;
use orbit_automation::routines::install_clock;
use orbit_registry::load_host_identity;

#[derive(Args)]
#[command(
    after_help = "Reads the machine identity from the selected Orbit root's host.toml (created by\n\
                        `orbit init`) and, with --install-clock, installs the per-user OS\n\
                        clock unit that invokes `orbit sweep` every minute (launchd on\n\
                        macOS, a systemd user timer on Linux). It never creates or rewrites\n\
                        host identity — run `orbit init` for that."
)]
pub struct RoutineInitArgs {
    /// Also install and activate the OS clock unit driving `orbit sweep`.
    #[arg(long)]
    pub install_clock: bool,
}

impl RoutineInitArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        // Read-only: host identity is owned by `orbit init`. Fail closed with
        // an actionable error when it is absent or unmigrated.
        let identity = load_host_identity(global_root)?;
        println!(
            "host identity: host_id=\"{}\", machine_id={}",
            identity.host_id, identity.machine_id
        );

        if !self.install_clock {
            println!("clock unit not installed (pass --install-clock to set up `orbit sweep`)");
            return Ok(CommandOutput::Silent);
        }

        let report = install_clock(global_root)?;
        for file in &report.files_written {
            println!("wrote {}", file.display());
        }
        if report.activated {
            println!("clock unit active: `orbit sweep` runs every minute on this host");
        } else {
            println!("clock unit files written but not activated; run:");
            for step in &report.manual_steps {
                println!("  {step}");
            }
        }
        Ok(CommandOutput::Silent)
    }
}
