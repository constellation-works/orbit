//! The crew memory app: a run's read, write and proposal of its crew's
//! memory.
//!
//! [`Command`] is a verb of the memory noun in the operation registry
//! (`orbit_common::governance::memory`) with its fields, named and typed as
//! the verb's spec declares; its [`Outcome`] carries the JSON that spec
//! documents. A command from a surface still runs through the generic tool
//! entry, which checks the fields against the spec, decides the tool row and
//! reaches the memory tool handler. The handler calls [`exec`] again from
//! inside that entry, and that call runs the flow with the caller's trusted
//! run binding.

mod agent;

mod delivery;

mod dream;

mod key;

mod operation;

mod proposal;

mod record;

mod space;

mod validate;

use orbit_common::OrbitError;
use orbit_common::governance::memory::MemoryVerb;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
#[cfg(not(feature = "test-support"))]
use agent::MemoryToolCaller;
pub(crate) use agent::crew_memory_tool_suppressions;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use agent::{MemoryToolCaller, crew_memory_tool_show};
pub(crate) use delivery::render_crew_memory_delivery;
pub(crate) use dream::{CREW_MEMORY_DREAM_TAG, crew_memory_dream};
pub(crate) use key::{MemoryKey, memory_key, parse_memory_key};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use proposal::crew_memory_proposal_board;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use record::{decide_proposal, open_proposal};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use space::crew_memory_write;
pub(crate) use validate::{
    GLOBAL_MAX_BYTES, GLOBAL_MAX_FILES, MODEL_SPACE_MAX_BYTES, MODEL_SPACE_MAX_FILES,
    MemoryLintVocabulary, MemoryWriteContext, validate_memory_write,
};

/// One Markdown file of a memory space.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryFile {
    /// The file name, `[a-z0-9-]{1,40}.md`.
    pub path: String,
    pub text: String,
}

/// A write's whole new content and the evidence it cites. A write replaces
/// every file of the space; there is no append.
#[doc(hidden)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryWrite {
    pub files: Vec<MemoryFile>,
    /// Evidence task ids the notes rest on.
    pub evidence: Vec<String>,
}

/// Who writes a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryWriter {
    /// An operator. May cite no evidence.
    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    Operator { actor: String },
    /// A dreaming task's run (slice M3): it must cite evidence from its
    /// evidence pack, and it writes only its task's `crew_memory_key`.
    Run {
        run_id: String,
        crew: String,
        actor: String,
    },
}

impl MemoryWriter {
    /// `operator` or `run`, as stored.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Operator { .. } => "operator",
            Self::Run { .. } => "run",
        }
    }

    /// The label stored as the revision's and event's actor.
    pub(crate) fn actor(&self) -> &str {
        match self {
            Self::Operator { actor } | Self::Run { actor, .. } => actor,
        }
    }
}

/// One memory verb and the fields it is called with.
pub struct Command {
    verb: MemoryVerb,
    input: Input,
}

impl Command {
    /// The command for `verb`, called with `input`.
    pub fn new(verb: MemoryVerb, input: Input) -> Self {
        Self { verb, input }
    }
}

/// What a command produced: the JSON its verb's spec documents.
pub struct Outcome(Value);

impl Outcome {
    /// The JSON this outcome carries.
    pub fn into_value(self) -> Value {
        self.0
    }
}

/// Why a crew memory command failed.
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

struct CrewMemoryApp;

impl App for CrewMemoryApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        // The generic tool entry decides the tool row.
        Operation::of(command.verb, &command.input.fields).without_tool_row()
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let Command { verb, input } = command;
        let value = tool_call::run(core, verb, input, |runtime, input, caller| {
            operation::dispatch(runtime, verb, input, caller)
        })?;
        Ok(Outcome(value))
    }
}

/// Run one crew memory command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<CrewMemoryApp>(core, command)
}
