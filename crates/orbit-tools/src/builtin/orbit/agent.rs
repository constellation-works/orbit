//! Operator-only host agent invocation [ORB-11354].
//!
//! Submits one durable, asynchronous exploration run and returns its run ID.
//! Everything that decides whether it may run lives below this file: the
//! governed-operation chokepoint in
//! `orbit_common::governance::authorization` gates the tool name, and
//! `OrbitRuntime::admit_agent_invoke` is the canonical admission that also
//! covers the CLI. This tool collects input and hands it over.
//!
//! The self-dispatch guard below mirrors `orbit.command.exec`: a managed run's
//! leaf agent must not reach a surface whose entire purpose is to start a
//! process outside the sandbox that run is executing inside. The capability
//! chokepoint would refuse it anyway — a managed run resolves as an agent —
//! but refusing by run scope first gives the leaf the reason rather than a
//! capability message that reads like a misconfiguration.

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
                     Orbit task, lifecycle, commit, or pull-request change."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "cwd".to_string(),
                description: "Absolute working directory the agent starts in. Must exist and be \
                     inside this workspace's checkout; it is never inferred from the caller."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "crew".to_string(),
                description: "Configured crew selecting the provider, model, and reasoning \
                     effort. Defaults to the workspace's default crew."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "timeout_seconds".to_string(),
                description: "Wall-clock bound for the invocation. Defaults to 1800 and may not \
                     exceed 7200."
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
        ];
        parameters.extend(super::model_identity_params());
        ToolSchema {
            name: "orbit.agent.invoke".to_string(),
            description:
                "Submit an asynchronous agent invocation for exploration or debugging and return \
                 its run ID. The agent runs on the host outside Orbit's filesystem sandbox, as \
                 the same OS user as Orbit, so it can reach anything that user can; it is \
                 admitted per invocation and requires operator capability. Remote callers also \
                 require an explicit destination-owned, workspace-scoped `agent_invoke` grant; \
                 its default mode requires a key-bound identity, while an explicit cooperative \
                 mode trusts the same-OS-account SSH operator channel and records identity as \
                 self-asserted. Track it with \
                 `orbit.workflow.run.show`, read output with `orbit run logs <RUN_ID>`, and stop \
                 it with `orbit run cancel <RUN_ID>`. It changes no task, opens no pull request, \
                 and dispatches nothing."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        if ctx
            .orbit_host
            .as_ref()
            .is_some_and(|host| host.task_scope().run_id.is_some())
        {
            return Err(OrbitError::CapabilityDenied(
                "managed runs cannot invoke a host agent; a leaf agent admitting an unsandboxed \
                 subprocess would step outside the sandbox its own run executes inside"
                    .to_string(),
            ));
        }
        super::execute_host_action(ctx, input, OrbitBuiltinAction::AgentInvoke)
    }
}
