//! The notice app: post, close, list and show fleet notices.
//!
//! [`Command::Operation`] is a verb of the notice noun in the operation
//! registry (`orbit_common::governance::notice`) with its fields, named and
//! typed as the verb's spec declares; its [`Outcome::Value`] carries the JSON
//! that spec documents. Such a command from a surface still runs through the
//! generic tool entry, which checks the fields against the spec, decides the
//! tool row and reaches the notice tool handler. The handler calls [`exec`]
//! again from inside that entry, and that call runs the flow.
//!
//! The dashboard's list, post and close are typed commands: the list's
//! [`Outcome`] is the effective-state listing, and a write's is the record it
//! wrote. Each write records its own notice audit row, as it did before.

mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::notice::NoticeVerb;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
pub use crate::application::notice::{
    NoticeCaller, NoticeCloseResult, NoticeListFilter, NoticeListing, NoticePostRawFields,
    NoticePostRequest, NoticePostResult, NoticeSelector, NoticeStatusFilter,
};

/// What the notice app can be asked to do.
pub enum Command {
    /// A registry verb of the notice noun and the fields it is called with.
    Operation(NoticeVerb, Input),
    /// The dashboard's read of notices' effective state.
    List(NoticeListFilter),
    /// The dashboard's post, from a form whose blank fields mean absent.
    Post {
        /// Who posts.
        caller: NoticeCaller,
        /// The form's fields.
        fields: NoticePostRawFields,
    },
    /// The dashboard's close of one notice.
    Close {
        /// Who closes.
        caller: NoticeCaller,
        /// The notice's id.
        id: String,
        /// Why it is closed; empty for no reason.
        reason: String,
    },
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents.
    Value(Value),
    /// The notices the filter selected.
    Listed(NoticeListing),
    /// The notice a post wrote or matched.
    Posted(NoticePostResult),
    /// The notice a close ended.
    Closed(NoticeCloseResult),
}

impl Outcome {
    /// The JSON a registry verb's spec documents.
    pub fn into_value(self) -> Result<Value, crate::Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Listed(_) | Self::Posted(_) | Self::Closed(_) => Err(OrbitError::Execution(
                "notice dashboard outcome has no registry document".to_string(),
            )),
        }
    }
}

/// Why a notice command failed.
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

struct NoticeApp;

impl App for NoticeApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            Command::List(_) => Operation::dashboard_read("notice", "list"),
            // The dashboard decided its operator session before it sent the
            // write, and the write records its own notice audit row.
            Command::Post { .. } => Operation::dashboard_read("notice", "post"),
            Command::Close { .. } => Operation::dashboard_read("notice", "close"),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::Operation(verb, input) => Outcome::Value(tool_call::run(
                core,
                verb,
                input,
                |runtime, input, caller| operation::dispatch(runtime, verb, input, caller),
            )?),
            Command::List(filter) => Outcome::Listed(runtime.list_notices(&filter)?),
            Command::Post { caller, fields } => {
                let request = NoticePostRequest::parse_raw(fields)?;
                Outcome::Posted(runtime.post_notice(&caller, request)?)
            }
            Command::Close { caller, id, reason } => {
                Outcome::Closed(runtime.close_notice(&caller, NoticeSelector::Id(id), &reason)?)
            }
        })
    }
}

/// Run one notice command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<NoticeApp>(core, command)
}
