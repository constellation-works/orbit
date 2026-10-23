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
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::super::operation_args::Invocation;
use super::prepare::prepare;
use super::render::render;
use crate::command::{CommandOut, Execute};

#[derive(Args)]
#[command(about = orbit_common::governance::auto_task::AUTO_TASK.cli_about)]
pub struct AutoTaskCommand {
    #[command(subcommand)]
    pub command: Invocation<AutoTaskVerb>,
}

impl Execute for AutoTaskCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        let filter = prepare(spec.verb, &mut input)?;
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        render(spec.verb, &name, value, filter)
    }
}
