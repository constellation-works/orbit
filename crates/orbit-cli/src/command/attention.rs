//! The root attention CLI derives its inputs from the shared tool operation.
use super::{
    CommandOut, ExecuteCore,
    operation_args::{Invocation, augment_operation_args, input_from_matches},
};

use clap::{Arg, ArgMatches, Args, Command, FromArgMatches};
use orbit_common::governance::{attention::AttentionVerb, operation::OperationVerb};
use orbit_core::Core;
use orbit_core::app::attention::{self, Command as AttentionAppCommand, Input, Outcome};

/// The timing of `orbit attention wait`.
pub struct AttentionWait {
    timeout: String,
    interval: Option<String>,
}

pub struct AttentionCommand {
    pub query: Invocation<AttentionVerb>,
    /// Set for `orbit attention wait`, whose filters are in `query.input`.
    pub wait: Option<AttentionWait>,
}
impl Args for AttentionCommand {
    fn augment_args(cmd: Command) -> Command {
        // The wait takes the read's own filters and cursor, then adds its timing.
        let wait = augment_operation_args(Command::new("wait"), AttentionVerb::Query.spec())
            // The grant bounds a wait to workspaces on this host.
            .mut_arg("all", |arg| arg.hide(true))
            .mut_arg("host", |arg| arg.hide(true))
            .about("Block until attention changes, with no model turn")
            .long_about(
                "Block until the attention delta since --since is non-empty or its degraded set \
                 changes, or the timeout passes.\n\nThe caller needs a grant from a session: call \
                 orbit.attention with wait_grant true over MCP and export the returned token as \
                 ORBIT_ATTENTION_WAIT_TOKEN. Without one the wait is refused.\n\nExit codes: 0 \
                 changed items (printed with the next cursor), 3 timeout with no change, 4 degraded \
                 set changed, 5 the final read failed: either every read failed, or only the last \
                 one failed after earlier reads succeeded. Exit 5 prints no envelope; the `wait` \
                 record carries the error. A timeout still lists degraded sources: it is not \
                 all-clear while any are listed.",
            )
            .arg(
                Arg::new("timeout")
                    .long("timeout")
                    .value_name("DURATION")
                    .default_value("30m")
                    .help("Longest to wait, such as 90s, 30m or 2h (max 24h)"),
            )
            .arg(
                Arg::new("interval")
                    .long("interval")
                    .value_name("DURATION")
                    .help("Gap between polls (default 15s, minimum 5s)"),
            );
        augment_operation_args(cmd, AttentionVerb::Query.spec()).subcommand(wait)
    }
    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}
impl FromArgMatches for AttentionCommand {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let spec = AttentionVerb::Query.spec();
        let wait = matches.subcommand_matches("wait");
        Ok(Self {
            query: Invocation {
                spec,
                input: input_from_matches(spec, wait.unwrap_or(matches)),
            },
            wait: wait.map(|wait| AttentionWait {
                timeout: wait
                    .get_one::<String>("timeout")
                    .cloned()
                    .unwrap_or_else(|| "30m".into()),
                interval: wait.get_one::<String>("interval").cloned(),
            }),
        })
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl ExecuteCore for AttentionCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let command = match self.wait {
            Some(wait) => AttentionAppCommand::Wait {
                fields: self.query.input,
                timeout: wait.timeout,
                interval: wait.interval,
            },
            None => AttentionAppCommand::Query(Input::new(self.query.input)),
        };
        match attention::exec(core, command)? {
            Outcome::Envelope(value) => crate::output::attention::envelope_output(value),
            Outcome::Waited(outcome) => crate::output::attention::wait_output(outcome),
        }
    }
}
