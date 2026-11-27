//! The friction app: report, list and triage friction records.
//!
//! [`Command`] is the typed form of the friction noun's verb enum in the
//! operation registry (`orbit_common::governance::friction`), which stays the
//! contract every surface shares: a command's fields are named and typed as
//! its verb's spec declares, and its [`Outcome`] carries the record JSON that
//! spec documents.
//!
//! A command from a surface still runs through the generic tool entry, which
//! checks the fields against the spec, routes a worker's call to its owner
//! and reaches the friction tool handler. The handler calls [`exec`] again
//! from inside that entry, and that call runs the flow. So every friction
//! entry ends in the flows below, and the surface's call is the one the
//! chokepoint audits.

mod add;
mod list;
mod record;
mod rehome;
mod resolve;
mod show;
mod stats;
mod tags;
mod update;

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_tools::ToolContext;
use serde_json::{Map, Value};

use super::exec::{self, App, Operation};
use crate::Core;
use crate::application::contracts::ToolInvocationPort;
use crate::composition::Entry;

// What the worker's owner-routed friction write shares with `add`.
pub(crate) use self::add::add_params;
pub(crate) use self::record::record_to_json_with_tag_normalizations;

/// What the friction app can be asked to do: one variant per registry verb.
pub enum Command {
    /// Append a friction report.
    Add(Input),
    /// List friction records.
    List(Input),
    /// Show one friction record.
    Show(Input),
    /// Compute friction rates.
    Stats(Input),
    /// List configured taxonomy tags.
    Tags(Input),
    /// Update triage metadata.
    Update(Input),
    /// Mark a record resolved.
    Resolve(Input),
    /// Move a record into the workspace that owns it.
    Rehome(Input),
    /// Read the workspace's configured friction tag taxonomy for MCP schema decoration.
    TagTaxonomy,
}

impl Command {
    /// The command for `verb`, called with `input`.
    pub fn new(verb: FrictionVerb, input: Input) -> Self {
        match verb {
            FrictionVerb::Add => Self::Add(input),
            FrictionVerb::List => Self::List(input),
            FrictionVerb::Show => Self::Show(input),
            FrictionVerb::Stats => Self::Stats(input),
            FrictionVerb::Tags => Self::Tags(input),
            FrictionVerb::Update => Self::Update(input),
            FrictionVerb::Resolve => Self::Resolve(input),
            FrictionVerb::Rehome => Self::Rehome(input),
        }
    }

    fn verb(&self) -> Option<FrictionVerb> {
        match self {
            Self::Add(_) => Some(FrictionVerb::Add),
            Self::List(_) => Some(FrictionVerb::List),
            Self::Show(_) => Some(FrictionVerb::Show),
            Self::Stats(_) => Some(FrictionVerb::Stats),
            Self::Tags(_) => Some(FrictionVerb::Tags),
            Self::Update(_) => Some(FrictionVerb::Update),
            Self::Resolve(_) => Some(FrictionVerb::Resolve),
            Self::Rehome(_) => Some(FrictionVerb::Rehome),
            Self::TagTaxonomy => None,
        }
    }

    fn input(&self) -> Option<&Input> {
        match self {
            Self::Add(input)
            | Self::List(input)
            | Self::Show(input)
            | Self::Stats(input)
            | Self::Tags(input)
            | Self::Update(input)
            | Self::Resolve(input)
            | Self::Rehome(input) => Some(input),
            Self::TagTaxonomy => None,
        }
    }

    fn into_verb_input(self) -> Option<(FrictionVerb, Input)> {
        match self {
            Self::Add(input) => Some((FrictionVerb::Add, input)),
            Self::List(input) => Some((FrictionVerb::List, input)),
            Self::Show(input) => Some((FrictionVerb::Show, input)),
            Self::Stats(input) => Some((FrictionVerb::Stats, input)),
            Self::Tags(input) => Some((FrictionVerb::Tags, input)),
            Self::Update(input) => Some((FrictionVerb::Update, input)),
            Self::Resolve(input) => Some((FrictionVerb::Resolve, input)),
            Self::Rehome(input) => Some((FrictionVerb::Rehome, input)),
            Self::TagTaxonomy => None,
        }
    }
}

/// One command's fields, named as its verb's registry spec declares them,
/// and who the command is attributed to.
pub struct Input {
    fields: Value,
    reporter: Reporter,
}

/// Who a command is attributed to.
enum Reporter {
    /// Whoever the calling session resolves to.
    Session,
    /// A label the entry has already established, never read from the fields.
    Trusted(String),
}

impl Input {
    /// Fields from a caller the session identifies.
    pub fn new(fields: Map<String, Value>) -> Self {
        Self {
            fields: Value::Object(fields),
            reporter: Reporter::Session,
        }
    }

    /// Fields from a person operating the surface directly.
    pub fn from_human(fields: Map<String, Value>) -> Self {
        Self {
            fields: Value::Object(fields),
            reporter: Reporter::Trusted("human".to_string()),
        }
    }

    /// The input and the caller's model that the generic tool entry hands a
    /// tool handler.
    pub(crate) fn from_tool_entry(input: Value, model: Option<String>) -> Self {
        Self {
            fields: input,
            reporter: model.map_or(Reporter::Session, Reporter::Trusted),
        }
    }
}

/// What a command produced: the record JSON its verb's spec documents.
pub enum Outcome {
    /// The record `add` stored.
    Added(Value),
    /// The records `list` matched.
    Listed(Value),
    /// The record `show` found.
    Shown(Value),
    /// The rates `stats` computed.
    Stats(Value),
    /// The tags `tags` listed.
    Tags(Value),
    /// The record `update` changed.
    Updated(Value),
    /// The record `resolve` closed.
    Resolved(Value),
    /// The record `rehome` moved.
    Rehomed(Value),
    /// The workspace's friction tag taxonomy.
    TagTaxonomy(Vec<(String, String)>),
}

impl Outcome {
    fn new(verb: FrictionVerb, value: Value) -> Self {
        match verb {
            FrictionVerb::Add => Self::Added(value),
            FrictionVerb::List => Self::Listed(value),
            FrictionVerb::Show => Self::Shown(value),
            FrictionVerb::Stats => Self::Stats(value),
            FrictionVerb::Tags => Self::Tags(value),
            FrictionVerb::Update => Self::Updated(value),
            FrictionVerb::Resolve => Self::Resolved(value),
            FrictionVerb::Rehome => Self::Rehomed(value),
        }
    }

    /// The JSON this outcome carries.
    pub fn into_value(self) -> Value {
        match self {
            Self::Added(value)
            | Self::Listed(value)
            | Self::Shown(value)
            | Self::Stats(value)
            | Self::Tags(value)
            | Self::Updated(value)
            | Self::Resolved(value)
            | Self::Rehomed(value) => value,
            Self::TagTaxonomy(_) => Value::Null,
        }
    }

    /// The friction tag taxonomy answered by [`Command::TagTaxonomy`].
    pub fn into_tag_taxonomy(self) -> Result<Vec<(String, String)>, crate::Error> {
        match self {
            Self::TagTaxonomy(taxonomy) => Ok(taxonomy),
            _ => Err(OrbitError::Execution(
                "friction command answered an outcome other than TagTaxonomy".to_string(),
            )),
        }
    }
}

/// Why a friction command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flow, or the tool entry in front of it, refused or failed the
    /// command. The friction code the flows call reports the shared error.
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

struct FrictionApp;

impl App for FrictionApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match (command.verb(), command.input()) {
            (Some(verb), Some(input)) => Operation::of(verb, &input.fields),
            _ => Operation::dashboard_read("friction", "tag-taxonomy"),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        let Some((verb, Input { fields, reporter })) = command.into_verb_input() else {
            return Ok(Outcome::TagTaxonomy(runtime.friction_tag_taxonomy()?));
        };
        let value = match core.entry() {
            // The handler table: exhaustive, so a new verb has no flow until
            // it is given one. The tool entry checked the fields against the
            // verb's spec, so the flows read canonical parameter names and
            // keep only content rules.
            Entry::Tool => {
                let model = match reporter {
                    Reporter::Session => None,
                    Reporter::Trusted(label) => Some(label),
                };
                if matches!(verb, FrictionVerb::Add | FrictionVerb::Rehome) {
                    runtime.ensure_coordination_task_write_permitted()?;
                }
                // Re-wraps the input the tool entry already parsed.
                let input = verb.spec().parse_input(fields)?;
                match verb {
                    FrictionVerb::Add => add::add(runtime, &input, model),
                    FrictionVerb::List => list::list(runtime, &input),
                    FrictionVerb::Show => show::show(runtime, &input),
                    FrictionVerb::Stats => stats::stats(runtime),
                    FrictionVerb::Tags => tags::tags(runtime),
                    FrictionVerb::Update => update::update(runtime, &input),
                    FrictionVerb::Resolve => resolve::resolve(runtime, &input),
                    FrictionVerb::Rehome => rehome::rehome(runtime, &input),
                }?
            }
            Entry::CommandLine | Entry::Service => {
                // The trusted label lives in the tool context, never in the
                // fields a caller controls.
                let tool_context = ToolContext {
                    trusted_actor_label: match reporter {
                        Reporter::Session => None,
                        Reporter::Trusted(label) => Some(label),
                    },
                    ..Default::default()
                };
                ToolInvocationPort::run_tool_with_context(
                    runtime,
                    verb.tool_name(),
                    fields,
                    tool_context,
                )?
            }
        };
        Ok(Outcome::new(verb, value))
    }
}

/// Run one friction command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<FrictionApp>(core, command)
}
