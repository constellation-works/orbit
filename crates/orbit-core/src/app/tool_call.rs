//! What the apps of registry nouns share while their commands still pass
//! through the generic tool entry: one command's fields, who sent it, and
//! the two ways a command reaches its flow.

use orbit_common::OrbitError;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_tools::{ActivityBinding, ToolContext};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Map, Value};

use super::exec;
use crate::application::contracts::ToolInvocationPort;
use crate::{Core, OrbitRuntime};

/// One command's fields, named as its verb's registry spec declares them,
/// and the caller it is decided for.
pub struct Input {
    pub(crate) fields: Value,
    caller: Box<Caller>,
}

impl Input {
    /// Fields from a caller the process identifies.
    pub fn new(fields: Map<String, Value>) -> Self {
        Self {
            fields: Value::Object(fields),
            caller: Box::default(),
        }
    }

    /// Fields from a dashboard session. The session carries the operator
    /// capability only when `operator` says the server holds it, never the
    /// hosting process's terminal or environment.
    pub fn from_dashboard(fields: Map<String, Value>, operator: bool) -> Self {
        Self {
            fields: Value::Object(fields),
            caller: Box::new(Caller {
                session: dashboard_session(operator),
                ..Caller::default()
            }),
        }
    }

    /// The input and caller the generic tool entry hands a tool handler.
    pub(crate) fn from_tool_entry(input: Value, caller: Caller) -> Self {
        Self {
            fields: input,
            caller: Box::new(caller),
        }
    }
}

/// The caller the generic tool entry resolved before it reached a handler.
#[derive(Default)]
pub(crate) struct Caller {
    pub(crate) session: ToolSessionContext,
    pub(crate) run_id: Option<String>,
    /// The task the tool context is scoped to, if any.
    pub(crate) task_id: Option<String>,
    /// The managed activity the dispatching host attested for this call.
    /// Only the tool host sets it; tool input and the process environment
    /// never do.
    pub(crate) activity: Option<ActivityBinding>,
    pub(crate) agent: Option<String>,
    pub(crate) model: Option<String>,
}

/// The session the dashboard calls a registry tool with.
pub(crate) fn dashboard_session(operator: bool) -> ToolSessionContext {
    let mut session = ToolSessionContext::default();
    session.effective_capabilities.insert(McpCapability::Agent);
    if operator {
        session
            .effective_capabilities
            .insert(McpCapability::Operator);
    }
    session
}

/// Run `verb` with `input`. A command from a surface goes through the generic
/// tool entry; the noun's tool handler calls its app's `exec` again from
/// inside that entry, and that call arrives here and runs `flow`.
pub(crate) fn run<V: OperationVerb>(
    core: &Core,
    verb: V,
    input: Input,
    flow: impl FnOnce(&OrbitRuntime, &OperationInput, &Caller) -> Result<Value, OrbitError>,
) -> Result<Value, OrbitError> {
    match core.caller() {
        exec::Caller::Tool => {
            let Input { fields, caller } = input;
            // Re-wraps the input the tool entry already parsed.
            flow(core.runtime(), &verb.spec().parse_input(fields)?, &caller)
        }
        exec::Caller::CommandLine | exec::Caller::Session(_) => {
            through_tool_entry(core.runtime(), verb, input)
        }
    }
}

/// Send `verb` through the generic tool entry, which checks the fields
/// against the spec, decides the tool row for the input's session and
/// reaches the noun's tool handler.
pub(crate) fn through_tool_entry<V: OperationVerb>(
    runtime: &OrbitRuntime,
    verb: V,
    input: Input,
) -> Result<Value, OrbitError> {
    ToolInvocationPort::run_tool_with_context(
        runtime,
        verb.tool_name(),
        input.fields,
        ToolContext {
            session_context: input.caller.session,
            ..Default::default()
        },
    )
}

/// [`run`] for a noun whose flow reads the tool entry's canonical input as it
/// arrived, without parsing it against the spec a second time.
pub(crate) fn run_unparsed<V: OperationVerb>(
    core: &Core,
    verb: V,
    input: Input,
    flow: impl FnOnce(&OrbitRuntime, Value, &Caller) -> Result<Value, OrbitError>,
) -> Result<Value, OrbitError> {
    match core.caller() {
        exec::Caller::Tool => {
            let Input { fields, caller } = input;
            flow(core.runtime(), fields, &caller)
        }
        exec::Caller::CommandLine | exec::Caller::Session(_) => {
            through_tool_entry(core.runtime(), verb, input)
        }
    }
}
