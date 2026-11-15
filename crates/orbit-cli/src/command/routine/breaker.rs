use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::{Args, Subcommand};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_core::OrbitError;
use orbit_core::application::routines::{bypass_routine_breaker, reset_routine_breaker};
use serde_json::json;

#[derive(Args)]
#[command(
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "After 3 fires in a row fail with the same error class, the scheduler\n\
                  stops firing the routine on this host. It tries one trial fire after a\n\
                  backoff that grows each time a trial fails (15 minutes up to 4 hours),\n\
                  or at the first tick after Orbit is upgraded; a successful trial\n\
                  closes the breaker. Breakers are host-local, and every trip, trial,\n\
                  reset and bypass is recorded (see `orbit routine show <name>`)."
)]
pub struct RoutineBreakerArgs {
    #[command(subcommand)]
    pub command: RoutineBreakerSubcommand,
}

#[derive(Subcommand)]
pub enum RoutineBreakerSubcommand {
    /// Close a tripped routine's breaker now so the scheduler fires it as usual
    Reset(RoutineBreakerTarget),
    /// Let the scheduler fire a tripped routine once on its next tick
    Bypass(RoutineBreakerTarget),
}

#[derive(Args)]
pub struct RoutineBreakerTarget {
    /// Routine name.
    pub name: String,
}

impl RoutineBreakerArgs {
    /// `reset` or `bypass`, for the audit row.
    pub fn action(&self) -> &'static str {
        match self.command {
            RoutineBreakerSubcommand::Reset(_) => "reset",
            RoutineBreakerSubcommand::Bypass(_) => "bypass",
        }
    }

    /// The routine this breaker action names, for the audit row.
    pub fn name(&self) -> &str {
        match &self.command {
            RoutineBreakerSubcommand::Reset(target) | RoutineBreakerSubcommand::Bypass(target) => {
                &target.name
            }
        }
    }

    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        let (name, reset) = match self.command {
            RoutineBreakerSubcommand::Reset(target) => (target.name, true),
            RoutineBreakerSubcommand::Bypass(target) => (target.name, false),
        };
        // A breaker is recorded by name; refuse a misspelt one rather than
        // report that nothing was tripped.
        let report = routine_statuses(global_root)?;
        if !report
            .statuses
            .iter()
            .any(|status| status.routine.definition.name == name)
        {
            return Err(OrbitError::InvalidInput(format!(
                "no routine named '{name}' (see `orbit routine list`)"
            )));
        }
        let changed = if reset {
            reset_routine_breaker(global_root, &name, "human")?
        } else {
            bypass_routine_breaker(global_root, &name, "human")?
        };
        let text = match (reset, changed) {
            (true, true) => format!("reset the breaker of '{name}'; it fires as scheduled again"),
            (false, true) => format!("'{name}' fires once on the next scheduler tick"),
            (_, false) => format!("the breaker of '{name}' is not tripped on this host"),
        };
        Ok(Payload::detail(
            json!({
                "routine": name,
                "action": if reset { "reset" } else { "bypass" },
                "changed": changed,
            }),
            text,
        )
        .into())
    }
}
