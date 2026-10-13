//! The task verbs derived from the task operation registry.
//!
//! `list`, `eligible`, `show` and `lint` are declared once in
//! `orbit_common::governance::task::operations`; the clap tree is the
//! noun-agnostic adapter in [`super::super::operation_args`], and every verb
//! runs through the tool. The other task verbs stay hand-wired in
//! [`super::command`].
//!
//! What stays here is CLI-specific: [`prepare`] consumes the CLI-only flags,
//! and [`render`] builds the text output from the tool's answer. Both are
//! exhaustive matches on [`TaskVerb`].

use orbit_common::governance::task::TaskVerb;
use orbit_core::OrbitRuntime;
use serde_json::{Map, Value};

use super::super::operation_args::Invocation;
use crate::command::CommandOut;

/// How `list` renders its rows; consumed from CLI-only flags.
#[derive(Clone, Copy, Default)]
pub(super) struct ListView {
    /// Signal-tier records (`--ops`).
    pub(super) ops: bool,
    /// Every table column (`--full`).
    pub(super) full: bool,
}

pub(super) fn execute(invocation: Invocation<TaskVerb>, runtime: &OrbitRuntime) -> CommandOut {
    let Invocation { spec, mut input } = invocation;
    let view = prepare(spec.verb, &mut input);
    let value = runtime.run_tool(spec.tool_name, Value::Object(input.clone()))?;
    render(spec.verb, runtime, &input, value, view)
}

/// Shape a task request for the CLI before it reaches the tool. Each
/// CLI-only parameter is consumed here, so none reaches the tool.
fn prepare(verb: TaskVerb, input: &mut Map<String, Value>) -> ListView {
    match verb {
        TaskVerb::List => {
            // `--all` is a deprecated no-op.
            input.shift_remove("all");
            ListView {
                ops: take_switch(input, "ops"),
                full: take_switch(input, "full"),
            }
        }
        // `--host` was resolved by the host preflight before dispatch.
        TaskVerb::Show => {
            input.shift_remove("host");
            ListView::default()
        }
        TaskVerb::Eligible | TaskVerb::Lint => ListView::default(),
    }
}

fn take_switch(input: &mut Map<String, Value>, name: &str) -> bool {
    input.shift_remove(name) == Some(Value::Bool(true))
}

/// Build a task response's text payload.
fn render(
    verb: TaskVerb,
    runtime: &OrbitRuntime,
    input: &Map<String, Value>,
    value: Value,
    view: ListView,
) -> CommandOut {
    match verb {
        TaskVerb::List => super::list::render(input, value, view),
        TaskVerb::Eligible => super::eligible::render(input, value),
        TaskVerb::Show => super::show::render(runtime, input, value),
        TaskVerb::Lint => super::lint::render(input, value),
    }
}
