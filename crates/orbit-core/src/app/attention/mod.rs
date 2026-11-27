//! The attention app: what is waiting on the operator, and a wait that
//! blocks until that changes.
//!
//! [`Command::Query`] is the query verb of the attention noun in the
//! operation registry (`orbit_common::governance::attention`) with its
//! fields, named and typed as the verb's spec declares; its
//! [`Outcome::Envelope`] carries the JSON that spec documents. Such a command
//! from a surface still runs through the generic tool entry, which checks the
//! fields against the spec, decides the tool row and reaches the attention
//! tool handler. The handler calls [`exec`] again from inside that entry, and
//! that call runs the flow.
//!
//! [`Command::Wait`] polls the same read under a session's wait grant.
//!
//! The read itself is local scopes, source derivation and the destination
//! side of federation. Filters, the cursor and the public page live with the
//! operation in `orbit_common::governance::attention`, so the federated
//! aggregator applies the same rules once over every host.

mod drain_keeper_sources;

mod host;

mod notice_sources;

mod provider_sources;

mod query;

mod routine_sources;

mod runtime_sources;

mod sources;

mod wait;

mod wait_grant;

use orbit_common::OrbitError;
use orbit_common::governance::attention::AttentionVerb;
use serde_json::{Map, Value};

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
use query::{
    ATTENTION_AGE_BUMP_HOURS, ATTENTION_CI_CACHE_SECONDS, ATTENTION_RUN_STUCK_AFTER_MINUTES,
    ATTENTION_RUN_WINDOW_HOURS, ATTENTION_SOURCE_TIMEOUT_SECONDS,
};
pub(crate) use wait::wait_for_attention_delta;
pub use wait::{AttentionWaitOutcome, AttentionWaitStatus};
pub(crate) use wait_grant::AttentionWaitScope;

/// What the attention app can be asked to do.
pub enum Command {
    /// The attention read, called with its fields.
    Query(Input),
    /// Block until the read's delta is non-empty or its degraded set
    /// changes, or `timeout` passes. The wait grant is the one the process
    /// environment carries in `ORBIT_ATTENTION_WAIT_TOKEN`.
    Wait {
        /// The read's filters and cursor.
        fields: Map<String, Value>,
        /// The longest to wait.
        timeout: String,
        /// The gap between polls.
        interval: Option<String>,
    },
}

/// What a command produced.
pub enum Outcome {
    /// The attention envelope the read's spec documents.
    Envelope(Value),
    /// How the wait ended, with the last envelope it read.
    Waited(AttentionWaitOutcome),
}

/// Why an attention command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flow, or the tool entry in front of it, refused or failed the
    /// command.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct AttentionApp;

impl App for AttentionApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        // The generic tool entry decides the read's tool row; the wait calls
        // no tool. Both are audited as the read they repeat.
        match command {
            Command::Query(input) => {
                Operation::of(AttentionVerb::Query, &input.fields).without_tool_row()
            }
            Command::Wait { fields, .. } => {
                Operation::of(AttentionVerb::Query, &Value::Object(fields.clone()))
                    .without_tool_row()
            }
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        Ok(match command {
            Command::Query(input) => Outcome::Envelope(tool_call::run_unparsed(
                core,
                AttentionVerb::Query,
                input,
                |runtime, input, caller| {
                    let resident_scope =
                        runtime.resident_cycle_attention_scope(caller.run_id.as_deref());
                    let mint = input.get("wait_grant").and_then(Value::as_bool) == Some(true);
                    query::attention_query(runtime, input, &caller.session, resident_scope.as_ref())
                        .and_then(|response| {
                            if mint {
                                wait_grant::with_wait_grant(runtime, response)
                            } else {
                                Ok(response)
                            }
                        })
                },
            )?),
            Command::Wait {
                fields,
                timeout,
                interval,
            } => {
                let runtime = core.runtime();
                let token = runtime
                    .env()
                    .orbit_attention_wait_token()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or_default();
                Outcome::Waited(runtime.attention_wait(
                    token,
                    Value::Object(fields),
                    &timeout,
                    interval.as_deref(),
                )?)
            }
        })
    }
}

/// Run one attention command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<AttentionApp>(core, command)
}
