use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::{Args, Subcommand};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::OrbitError;
use orbit_core::app::routine::{
    RoutineBreakerAction, RoutineEditRefusal, control_listed_routine_breaker,
};
use serde_json::json;

#[derive(Args)]
#[command(
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "A tripped breaker stops the routine on this host until reset, a successful \
                  trial, or one bypass. Breakers are host-local.\n\n\
                  Examples:\n  \
                  orbit routine breaker reset <name>\n  \
                  orbit routine breaker bypass <name>",
    after_long_help = "After repeated fires fail with the same error class, the scheduler \
                       stops firing the routine on this host. A successful trial closes the \
                       breaker. Breakers are host-local, and every trip, trial, reset and \
                       bypass is recorded (`orbit routine show <name>`).\n\n\
                       The trip count and the backoff: installed orbit-setup skill \
                       `references/automation-scheduler.md`.\n\n\
                       Examples:\n  \
                       orbit routine breaker reset <name>\n  \
                       orbit routine breaker bypass <name>"
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
        let (name, action) = match self.command {
            RoutineBreakerSubcommand::Reset(target) => (target.name, RoutineBreakerAction::Reset),
            RoutineBreakerSubcommand::Bypass(target) => (target.name, RoutineBreakerAction::Bypass),
        };
        let report = routine_statuses(global_root)?;
        // The shared routine selection; a breaker is host-local, so no source
        // workspace narrows it. A misspelt name is refused rather than
        // reported as not tripped.
        let changed = control_listed_routine_breaker(
            orbit_core::app::workspace::store_opener(),
            global_root,
            &report,
            &name,
            None,
            action,
            "human",
        )?
        .map_err(|refusal| match refusal {
            RoutineEditRefusal::NotFound { .. } => OrbitError::InvalidInput(format!(
                "no routine named '{name}' (see `orbit routine list`)"
            )),
            other => other.into(),
        })?
        .changed();
        let text = match (action, changed) {
            (RoutineBreakerAction::Reset, true) => {
                format!("reset the breaker of '{name}'; it fires as scheduled again")
            }
            (RoutineBreakerAction::Bypass, true) => {
                format!("'{name}' fires once on the next scheduler tick")
            }
            (_, false) => format!("the breaker of '{name}' is not tripped on this host"),
        };
        Ok(Payload::detail(
            json!({
                "routine": name,
                "action": action.as_str(),
                "changed": changed,
            }),
            text,
        )
        .into())
    }
}
