use crate::OrbitRuntime;
use crate::application::task::decision_request_json;
use crate::application::task::{DecisionSubmitCaller, DecisionSubmitParams};
use crate::runtime::authorization::resolved_caller_capabilities;
use orbit_common::OrbitError;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_types::task::DecisionGate;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: DecisionVerb,
    input: Value,
    session_context: &ToolSessionContext,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        DecisionVerb::List => {
            let gate = input
                .optional_string("gate")?
                .map(|name| {
                    DecisionGate::from_name(&name).ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "unknown decision gate `{name}`; expected block_triage, pilot_admission, proposed_approval or review_approval"
                        ))
                    })
                })
                .transpose()?;
            let task_id = input.optional_string("task_id")?;
            let include_closed = input.optional_bool("include_closed")?.unwrap_or(false);
            let requests =
                runtime.list_decision_requests(gate, task_id.as_deref(), include_closed)?;
            Ok(json!({
                "requests": requests.iter().map(decision_request_json).collect::<Vec<_>>(),
                "count": requests.len(),
            }))
        }
        DecisionVerb::Show => {
            let id = input.required_string("id")?;
            let request = runtime.get_decision_request(&id)?.ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "request_not_found: no decision request `{id}` in this workspace"
                ))
            })?;
            Ok(decision_request_json(&request))
        }
        DecisionVerb::Digest => {
            let caller = submit_caller(runtime, session_context, agent, model)?;
            let digest =
                runtime.decision_digest(&input.required_string("channel")?, caller)?;
            Ok(json!({
                "requests": digest.requests.iter().map(decision_request_json).collect::<Vec<_>>(),
                "raised": digest.raised.iter().map(decision_request_json).collect::<Vec<_>>(),
                "expiring": digest.expiring.iter().map(decision_request_json).collect::<Vec<_>>(),
                "count": digest.requests.len(),
            }))
        }
        DecisionVerb::Submit => {
            let caller = submit_caller(runtime, session_context, agent, model)?;
            let request = runtime.submit_decision_via_channel(DecisionSubmitParams {
                channel: input.required_string("channel")?,
                request_id: input.required_string("id")?,
                answer_id: input.required_string("answer")?,
                note: input.optional_string("note")?,
                token: input.required_string("token")?,
                sender: input.optional_string("sender")?,
                auth_results: input.optional_string("auth_results")?,
                caller,
            })?;
            Ok(decision_request_json(&request))
        }
        // The tool entry refuses these verbs before dispatch and registers
        // them inactive; refusing here too keeps the invariant in core.
        DecisionVerb::Answer => Err(OrbitError::PolicyDenied(
            "answering a decision request is the operator's: use `orbit decision answer` or the dashboard".into(),
        )),
        DecisionVerb::ChannelRegister
        | DecisionVerb::ChannelList
        | DecisionVerb::ChannelRevoke
        | DecisionVerb::ChannelRotateKey => Err(OrbitError::PolicyDenied(
            "decision channels are registered from the operator CLI: use `orbit decision channel register|list|revoke|rotate-key`".into(),
        )),
    }
}

/// Who is submitting, from trusted seams only: the host-issued plugin
/// callback session this process inherited, or the caller's resolved
/// capability. Tool input never names its own caller. A caller carrying
/// an agent identity is an agent even on an operator-capable session —
/// the governed row already refused plain agent sessions, and this
/// derivation is the handler's own statement of the same rule.
fn submit_caller(
    runtime: &OrbitRuntime,
    session_context: &ToolSessionContext,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<DecisionSubmitCaller, OrbitError> {
    use orbit_tools::plugin::{CallbackResolution, resolve_plugin_callback_session};

    // A claimed or remote run never submits a decision (design §1): a
    // worker-bound session is an agent for this derivation whatever else
    // this process inherited.
    if session_context.worker_invocation.is_some() {
        return Ok(DecisionSubmitCaller::Agent);
    }
    if let CallbackResolution::Identified(identity) =
        resolve_plugin_callback_session(&runtime.global_root())?
    {
        return Ok(DecisionSubmitCaller::PluginCallback {
            plugin: identity.provenance.name,
        });
    }
    if agent.is_some() || model.is_some() {
        return Ok(DecisionSubmitCaller::Agent);
    }
    let capabilities = resolved_caller_capabilities(session_context);
    if capabilities.contains(&McpCapability::Operator) {
        return Ok(DecisionSubmitCaller::Operator);
    }
    if capabilities.contains(&McpCapability::Agent) {
        return Ok(DecisionSubmitCaller::Agent);
    }
    Ok(DecisionSubmitCaller::Unknown)
}
