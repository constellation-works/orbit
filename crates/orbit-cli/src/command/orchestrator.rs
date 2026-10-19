//! The deprecated `orbit orchestrator` command, derived from the orchestrator
//! operation registry.
//!
//! Both verbs call the `orbit.orchestrator.*` aliases, which answer the agents
//! scorecard for the orchestrators role with a `deprecation` note. The note
//! goes to stderr so `--json` stays the payload; the text views are
//! `orbit agents`'s.

use clap::Args;
use orbit_common::governance::orchestrator::OrchestratorVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::agents::{scorecard_blocks, show_blocks};
use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::orchestrator::ORCHESTRATOR.cli_about)]
pub struct OrchestratorCommand {
    #[command(subcommand)]
    pub command: Invocation<OrchestratorVerb>,
}

impl Execute for OrchestratorCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        if let Some(note) = value["deprecation"].as_str() {
            eprintln!("warning: {note}");
        }
        let blocks = match spec.verb {
            OrchestratorVerb::Scorecard => scorecard_blocks(&value),
            OrchestratorVerb::Show => show_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}
