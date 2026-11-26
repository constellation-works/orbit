use clap::Args;
use orbit_common::governance::friction::FrictionVerb;
use orbit_core::Core;
use orbit_core::app::friction::{self, Command, Input, Outcome};
use serde_json::{Map, Value};

use super::{add, list, rehome, resolve, show, stats, tags, update};
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
#[command(about = orbit_common::governance::friction::FRICTION.cli_about)]
pub struct FrictionCommand {
    #[command(subcommand)]
    pub command: Invocation<FrictionVerb>,
}

impl ExecuteCore for FrictionCommand {
    fn execute(self, core: &Core) -> CommandOut {
        self.command.require_at_least_one_of()?;
        let Invocation { spec, mut input } = self.command;
        prepare(spec.verb, &mut input)?;
        let value = friction::exec(core, Command::new(spec.verb, Input::new(input)))
            .map(Outcome::into_value)?;
        render(spec.verb, &value)
    }
}

/// Shape a friction request for the CLI before it reaches the tool.
fn prepare(verb: FrictionVerb, input: &mut Map<String, Value>) -> Result<(), orbit_core::Error> {
    match verb {
        FrictionVerb::List => list::prepare(input),
        FrictionVerb::Add
        | FrictionVerb::Show
        | FrictionVerb::Stats
        | FrictionVerb::Tags
        | FrictionVerb::Update
        | FrictionVerb::Resolve
        | FrictionVerb::Rehome => Ok(()),
    }
}

/// Build a friction response's text payload.
fn render(verb: FrictionVerb, value: &Value) -> CommandOut {
    match verb {
        FrictionVerb::Add => add::render(value),
        FrictionVerb::Show => show::render(value),
        FrictionVerb::Update => update::render(value),
        FrictionVerb::Resolve => resolve::render(value),
        FrictionVerb::Rehome => rehome::render(value),
        FrictionVerb::List => list::render(value),
        FrictionVerb::Tags => tags::render(value),
        // Aggregate rates have no useful flat rendering.
        FrictionVerb::Stats => stats::render(value),
    }
}
