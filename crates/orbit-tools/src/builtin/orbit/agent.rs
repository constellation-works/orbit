//! Agent invocation [ORB-11354] [ORB-16814].
//!
//! Submits one durable, asynchronous invocation run and returns its run ID.
//! Everything that decides whether it may run lives below this file: the
//! governed-operation chokepoint in
//! `orbit_common::governance::authorization` gates the tool name, and
//! `OrbitRuntime::admit_agent_invoke` is the canonical admission. An
//! operator gets the trusted-host invocation; a managed implement or
//! review-repair run gets a sandboxed read-only explorer, and every other
//! managed run is refused there with the reason. This tool collects input
//! and hands it over.

use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAgentInvokeTool;

impl Tool for OrbitAgentInvokeTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "prompt".to_string(),
                description: "What to investigate. The invoked agent reports back; it makes no \
                     Orbit task, lifecycle, commit, or pull-request change. An explorer lists \
                     the paths it read in `answer.paths_read`."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "cwd".to_string(),
                description: "Absolute working directory the agent starts in. Must exist and be \
                     inside this workspace's checkout or a linked worktree under \
                     `.orbit/state/worktrees/`; it is never inferred from the caller. An \
                     explorer's must be inside the calling run's own worktree."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "crew".to_string(),
                description: "Configured crew selecting the provider, model, and reasoning \
                     effort. Defaults to the workspace's default crew. An explorer's must be in \
                     `workflow.purpose_crews.explorer`, which it is drawn from when omitted."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "timeout_seconds".to_string(),
                description: "Provider wall-clock bound in seconds, excluding queue time. \
                     Defaults to 1800; must be from 1 to 7200, the activity's enforced ceiling. \
                     An explorer defaults to, and may not exceed, \
                     `workflow.explore.timeout_seconds`."
                    .to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
            ToolParam {
                name: "wait_seconds".to_string(),
                description: "Wait up to this many seconds (integer from 0 to 600). Returns \
                     `answer` and `agent_invocation` if finished; otherwise returns the run ID \
                     and actual state so you can keep observing it. The deadline never cancels \
                     the run. Omit for an asynchronous submission."
                    .to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
            ToolParam {
                name: "idempotency_key".to_string(),
                description: "Retry handle. A resubmission carrying a key a recent submission \
                     already used resolves that run instead of starting a second agent."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "provider_sandbox".to_string(),
                description: "Per-invocation inner-sandbox override for the selected crew's \
                     provider (Codex: `read-only`, `workspace-write`, or \
                     `danger-full-access`). Tightens or names the provider sandbox for \
                     this run only; values the provider does not support are refused. \
                     The submission result reports the effective mode as \
                     `provider:mode`. Operator invocations only: an explorer is always \
                     read-only."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::model_identity_params());
        ToolSchema {
            name: "orbit.agent.invoke".to_string(),
            description:
                "Submit an asynchronous agent invocation for exploration or debugging and return \
                 its run ID, or wait for its answer with `wait_seconds`. Queued submissions \
                 include `queued: true`, a one-based `queue_position` and a warning. \
                 An operator's invocation runs on the host outside Orbit's filesystem sandbox, \
                 as the same OS user as Orbit, so it can reach anything that user can; it is \
                 admitted per invocation and requires operator capability — the same test for a \
                 session that arrived over SSH as for a local one. An implement or review-repair \
                 agent may instead delegate a read-only question when `workflow.explore.enabled` \
                 is on: it gets a sandboxed explorer from the explorer crew pool that reads its \
                 own worktree, cannot write or change Orbit state, and is bounded by per-run \
                 call, concurrency, timeout and answer-size budgets; every other agent is \
                 refused, and a refusal never stops the caller's run. Track it with \
                 `orbit.workflow.run.show`: `agent_invocation.answer` carries the agent's \
                 summary, findings, next_steps, any other result fields and its bounded final \
                 message, and `agent_invocation.progress` its latest message and last activity \
                 while it runs. Read the complete output with `orbit run logs <RUN_ID>` and stop \
                 it with `orbit run cancel <RUN_ID>`. It changes no task, opens no pull request, \
                 and dispatches nothing."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::execute_host_action(ctx, input, OrbitBuiltinAction::AgentInvoke)
    }

    fn input_schema(&self) -> Option<Value> {
        let mut schema = Value::Object(orbit_common::protocol::tool_schema::tool_input_schema(
            &self.schema(),
        ));
        schema["properties"]["timeout_seconds"]["minimum"] = serde_json::json!(1);
        schema["properties"]["timeout_seconds"]["maximum"] = serde_json::json!(7200);
        // No schema `default`: an operator's invocation defaults to 1800 seconds
        // and an explorer's to `workflow.explore.timeout_seconds`, so a client
        // filling one in would push an explorer past its ceiling.
        schema["properties"]["wait_seconds"]["minimum"] = serde_json::json!(0);
        schema["properties"]["wait_seconds"]["maximum"] = serde_json::json!(600);
        Some(schema)
    }
}
