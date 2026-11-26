use chrono::Utc;
use clap::Args;
use orbit_common::governance::notice::NoticeVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::close;
use super::list;
use super::post::{self, post_cli_input};
use super::show;
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::notice::NOTICE.cli_about)]
pub struct NoticeCommand {
    #[command(subcommand)]
    pub command: Invocation<NoticeVerb>,
}

impl Execute for NoticeCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        if spec.verb == NoticeVerb::Post {
            post_cli_input(&mut input)?;
        }
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        Ok(render_notice(spec.verb, value).into())
    }
}

/// The text rendering of one notice tool answer, shared by the local
/// command and a call host routing delivered to the workspace's owner.
pub(crate) fn render_notice(verb: NoticeVerb, value: Value) -> Payload {
    let blocks = match verb {
        NoticeVerb::List => vec![list::block(&value, Utc::now())],
        NoticeVerb::Post => vec![post::block(&value)],
        NoticeVerb::Close => vec![close::block(&value)],
        NoticeVerb::Show => vec![show::block(&value)],
    };
    Payload::blocks(value, blocks)
}
