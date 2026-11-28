use clap::Args;
use orbit_common::governance::decision::DecisionVerb;
use orbit_core::Core;
use orbit_core::app::task::{self, Command, Input};
use serde_json::Value;

use super::{
    answer, channel_list, channel_register, channel_revoke, channel_rotate_key, consolidate,
    digest, list, raise, reverse, rule_auto, show, submit,
};
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
#[command(about = orbit_common::governance::decision::DECISION.cli_about)]
pub struct DecisionCommand {
    #[command(subcommand)]
    pub command: Invocation<DecisionVerb>,
}

impl ExecuteCore for DecisionCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let answer_text = input
            .get("answer")
            .and_then(Value::as_str)
            .map(str::to_string);
        let outcome = task::exec(core, Command::Decision(spec.verb, Input::new(input)))?;
        match spec.verb {
            DecisionVerb::List => list::render(outcome),
            DecisionVerb::Show => show::render(outcome),
            DecisionVerb::Raise => raise::render(outcome),
            DecisionVerb::Answer => answer::render(outcome, answer_text),
            DecisionVerb::Reverse => reverse::render(outcome),
            DecisionVerb::RuleAuto => rule_auto::render(outcome),
            DecisionVerb::Consolidate => consolidate::render(outcome),
            DecisionVerb::Submit => submit::render(outcome),
            DecisionVerb::Digest => digest::render(outcome),
            DecisionVerb::PluginRaise => raise::render(outcome),
            DecisionVerb::ChannelRegister => channel_register::render(outcome),
            DecisionVerb::ChannelList => channel_list::render(outcome),
            DecisionVerb::ChannelRevoke => channel_revoke::render(outcome),
            DecisionVerb::ChannelRotateKey => channel_rotate_key::render(outcome),
        }
    }
}
