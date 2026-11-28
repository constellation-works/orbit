use clap::Args;
use orbit_common::governance::notice::NoticeVerb;
use orbit_core::Core;
use orbit_core::app::notice::{self, Command, Input};
use orbit_core::app::tool::{self, ToolEntryPoint};
use serde_json::Value;

use super::post::post_cli_input;
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
#[command(about = orbit_common::governance::notice::NOTICE.cli_about)]
pub struct NoticeCommand {
    #[command(subcommand)]
    pub command: Invocation<NoticeVerb>,
}

impl ExecuteCore for NoticeCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        if spec.verb == NoticeVerb::Post {
            post_cli_input(&mut input)?;
        }
        let value = if matches!(spec.verb, NoticeVerb::Post | NoticeVerb::Close) {
            let (name, input) =
                orbit_types::tool::tool_family_call(spec.tool_name, Value::Object(input));
            tool::exec(
                core,
                tool::Command::Run {
                    name,
                    input,
                    agent: None,
                    model: None,
                    entry_point: ToolEntryPoint::Cli,
                    session: Default::default(),
                },
            )?
            .into_value()?
        } else {
            notice::exec(core, Command::Operation(spec.verb, Input::new(input)))?.into_value()?
        };
        Ok(crate::output::notice::render_notice(spec.verb, value).into())
    }
}
