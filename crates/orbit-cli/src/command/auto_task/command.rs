//! The auto-task CLI, derived from the auto-task operation registry.
//!
//! Flags, help text and input projection are declared once in
//! `orbit_common::governance::auto_task::operations`; the clap tree is the
//! noun-agnostic adapter in [`super::super::operation_args`]. Every verb runs
//! through the tool, including the CLI-only ones.
//!
//! What stays here is CLI-specific: [`prepare`] composes the per-field flags
//! into the tool's structured fields, and [`render`] builds the text output.
//! Both are exhaustive matches on [`AutoTaskVerb`].

use clap::Args;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_core::Core;
use orbit_core::app::auto_task::{self, Command, Input};
use serde_json::Value;

use super::super::operation_args::Invocation;
use super::prepare::prepare;
use super::render::render;
use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
#[command(about = orbit_common::governance::auto_task::AUTO_TASK.cli_about)]
pub struct AutoTaskCommand {
    #[command(subcommand)]
    pub command: Invocation<AutoTaskVerb>,
}

impl ExecuteCore for AutoTaskCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        let filter = prepare(spec.verb, &mut input)?;
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let value =
            auto_task::exec(core, Command::Operation(spec.verb, Input::new(input)))?.into_value();
        let value = if spec.verb == AutoTaskVerb::List {
            orbit_core::app::auto_task::AutoTaskListingProfile::Cli
                .project_auto_task_tool_document(value)
        } else {
            value
        };
        render(spec.verb, &name, value, filter)
    }
}
