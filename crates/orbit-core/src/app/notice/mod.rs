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

mod audit;

mod authority;

mod close;

mod limit;

mod operation;

mod post;

mod read;

#[cfg(feature = "test-support")]
mod test_support;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::governance::notice::{NOTICE_LIST_DEFAULT_LIMIT, NoticeVerb};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_types::record::{AuthorClass, EffectiveStatus, Notice, NoticeKind, NoticeScope};
use serde::Serialize;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
use audit::{NoticeAudit, notice_row, parse_notice_row};
pub use authority::NoticeCaller;
pub(crate) use close::TaskPort;
use close::linked_done_at;
#[cfg(not(feature = "test-support"))]
pub(crate) use close::run_notice_closure_tick;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use close::{close_notice, run_notice_closure_tick};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use limit::AGENT_POSTS_PER_TASK_PER_DAY;
pub(crate) use limit::{BRIEF_BODY_CHARS, BRIEF_MAX_ITEMS, BRIEF_READ_TIMEOUT_MS};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use post::post_notice;
pub(crate) use post::post_resident_control_line;
pub use post::{NoticePostRawFields, NoticePostRequest, NoticePostResult};
pub(crate) use read::{
    list_notices, notice_linked_task, resident_status_line, resident_status_lines,
};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use test_support::{with_notice_close_snapshot_hook, with_notice_post_time};

/// Done linked tasks by id, with the time each was last updated.
pub(crate) type LinkedDone = BTreeMap<String, DateTime<Utc>>;

/// Which notice a close names.
#[derive(Debug, Clone)]
pub enum NoticeSelector {
    Id(String),
    /// The open notice of the home workspace holding this key.
    DedupeKey(String),
}

/// What a close did.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeCloseResult {
    pub notice: Notice,
    /// The notice was already closed or expired; nothing was written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub already_inactive: bool,
}

/// Effective-state filter for a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeStatusFilter {
    #[default]
    Open,
    Closed,
    Expired,
    All,
}

impl NoticeStatusFilter {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "expired" => Some(Self::Expired),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    fn admits(self, status: EffectiveStatus) -> bool {
        match self {
            Self::Open => status == EffectiveStatus::Open,
            Self::Closed => status == EffectiveStatus::Closed,
            Self::Expired => status == EffectiveStatus::Expired,
            Self::All => true,
        }
    }
}

/// A listing's filters.
#[derive(Debug, Clone)]
pub struct NoticeListFilter {
    pub status: NoticeStatusFilter,
    pub include_global: bool,
    /// Read every workspace homed on this owner, not only the bound one:
    /// the owner-local aggregate a federated fan-out issues per owner.
    pub all_workspaces: bool,
    pub scope: Option<NoticeScope>,
    pub kinds: Vec<NoticeKind>,
    pub linked_task: Option<String>,
    pub author_class: Option<AuthorClass>,
    pub host: Option<String>,
    /// Closed or expired notices that ended before this instant are
    /// excluded; open notices are unaffected. `None` applies no window, so
    /// callers that need retained history keep it.
    pub ended_since: Option<DateTime<Utc>>,
    /// `None` returns every match; an aggregating caller applies its own
    /// limit once, after merging the owners' answers.
    pub limit: Option<usize>,
}

impl Default for NoticeListFilter {
    fn default() -> Self {
        Self {
            status: NoticeStatusFilter::Open,
            include_global: true,
            all_workspaces: false,
            scope: None,
            kinds: Vec::new(),
            linked_task: None,
            author_class: None,
            host: None,
            ended_since: None,
            limit: Some(NOTICE_LIST_DEFAULT_LIMIT),
        }
    }
}

/// A listing: the matching notices and the homes that could not be read.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeListing {
    pub notices: Vec<Notice>,
    pub degraded: Vec<String>,
}

/// One history entry of a notice, shown to the operator.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeHistoryEntry {
    pub at: DateTime<Utc>,
    pub action: String,
    pub poster_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_crew: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_task_id: Option<String>,
    pub detail: serde_json::Value,
}

/// One notice, with its history when the caller holds the operator
/// capability.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeView {
    pub notice: Notice,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<NoticeHistoryEntry>>,
}

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
            Command::List(filter) => Outcome::Listed(read::list_notices(runtime, &filter)?),
            Command::Post { caller, fields } => {
                let request = NoticePostRequest::parse_raw(fields)?;
                Outcome::Posted(post::post_notice(runtime, &caller, request)?)
            }
            Command::Close { caller, id, reason } => Outcome::Closed(close::close_notice(
                runtime,
                &caller,
                NoticeSelector::Id(id),
                &reason,
            )?),
        })
    }
}

/// Run one notice command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<NoticeApp>(core, command)
}

fn refuse(code: NoticeRefusalCode, message: impl Into<String>) -> OrbitError {
    OrbitError::notice_refused(code, message)
}
