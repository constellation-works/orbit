use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitTaskPullTool;

fn param(name: &str, param_type: &str, description: &str) -> ToolParam {
    ToolParam {
        name: name.to_string(),
        description: description.to_string(),
        param_type: param_type.to_string(),
        required: true,
    }
}

impl Tool for OrbitTaskPullTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }

    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            param(
                "request_id",
                "string",
                "Durable unique ID for one intended admission, persisted by the caller before \
                 sending. Reuse it unchanged after any uncertainty: a retry replays the stored \
                 receipt and never admits a second task.",
            ),
            param(
                "caller_version",
                "string",
                "The calling binary's version. It must equal the owner's.",
            ),
            param(
                "caller_schema",
                "integer",
                "The caller's distributed-drain wire-protocol schema version.",
            ),
            param(
                "caller_review_policy",
                "string",
                "The executor's effective review policy. Only `none` is admitted in v1.",
            ),
            param(
                "run_context",
                "object",
                "The calling drain: `run_id`, `job_name`, and an optional diagnostic \
                 `machine_name`.",
            ),
            param(
                "ship",
                "object",
                "The ship contract this owner resolves, exactly as `orbit.drain.probe` reported \
                 it. A new request carrying anything else is refused as \
                 `ship_contract_mismatch`, because the receipt freezes it.",
            ),
            ToolParam {
                required: false,
                ..param(
                    "crews",
                    "object",
                    "The crews the executor can run: `runnable` (its window preflight's crew \
                     names), `default_crew` (what a task naming no crew runs as there) and \
                     `excluded` (crews it cannot run, with why). A task whose crew it cannot \
                     run is skipped and stays in the backlog. Omitted: every crew is admissible.",
                )
            },
        ];
        ToolSchema {
            name: "orbit.task.pull".to_string(),
            description:
                "Distributed-drain admission, served by the workspace owner. Selects the first \
                 ready, valid, conflict-free backlog task in the owner's order and, in one \
                 transaction, reserves its footprint, records an execution claim for the \
                 calling machine, moves it to `in-progress` and stores the request receipt. \
                 Answers `idle` (a receipt with no claim) when nothing is admissible. The \
                 calling machine is the session's, never a field of this input. A claim grants \
                 execution of that one attempt only; completion still needs owner approval."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskPull)
    }
}
