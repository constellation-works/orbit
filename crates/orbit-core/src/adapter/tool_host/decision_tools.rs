use crate::OrbitRuntime;
use crate::application::task::decision_request_json;
use crate::application::task::{AgentDecisionRaise, DecisionSubmitCaller, DecisionSubmitParams};
use crate::runtime::authorization::resolved_caller_capabilities;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::{AgentDecisionOptionInput, DecisionGate};
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
                            "unknown decision gate `{name}`; expected agent_request, block_triage, pilot_admission, proposed_approval or review_approval"
                        ))
                    })
                })
                .transpose()?;
            let task_id = input.optional_string("task_id")?;
            let topic = input.optional_string("topic")?;
            let include_closed = input.optional_bool("include_closed")?.unwrap_or(false);
            let mut listing = runtime.list_decision_requests_with_warnings(gate, task_id.as_deref(), include_closed)?;
            if let Some(topic) = topic {
                listing
                    .requests
                    .retain(|request| request.topic.as_deref() == Some(topic.as_str()));
            }
            let mut value = json!({
                "requests": listing.requests.iter().map(decision_request_json).collect::<Vec<_>>(),
                "count": listing.requests.len(),
            });
            if !listing.warnings.is_empty() {
                value["warnings"] = json!(listing.warnings);
            }
            Ok(value)
        }
        DecisionVerb::Show => {
            let id = input.required_string("id")?;
            let request = runtime.get_decision_request(&id)?.ok_or_else(|| {
                OrbitError::decision_refused(
                    DecisionRefusalCode::RequestNotFound,
                    format!("no decision request `{id}` in this workspace"),
                )
            })?;
            Ok(decision_request_json(&request))
        }
        DecisionVerb::Raise => {
            let options = input
                .as_value()
                .get("options")
                .cloned()
                .ok_or_else(|| OrbitError::InvalidInput("`options` is required".into()))?;
            let options: Vec<AgentDecisionOptionInput> =
                serde_json::from_value(options).map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "`options` must be an array of {{label, consequence, recommended}} objects: {error}"
                    ))
                })?;
            let filer = runtime.actor().resolve_write_label(agent, model)?;
            let raised = runtime.raise_agent_decision_request(AgentDecisionRaise {
                task_id: input.required_string("task_id")?,
                question: input.required_string("question")?,
                options,
                topic: input.optional_string("topic")?,
                filer,
            })?;
            let mut value = decision_request_json(&raised.request);
            value["deduplicated"] = json!(raised.deduplicated);
            Ok(value)
        }
        DecisionVerb::Digest => {
            let caller = submit_caller(runtime, session_context, agent, model)?;
            let digest =
                runtime.decision_digest(&input.required_string("channel")?, caller)?;
            let mut value = json!({
                "requests": digest.requests.iter().map(decision_request_json).collect::<Vec<_>>(),
                "raised": digest.raised.iter().map(decision_request_json).collect::<Vec<_>>(),
                "expiring": digest.expiring.iter().map(decision_request_json).collect::<Vec<_>>(),
                "count": digest.requests.len(),
            });
            if !digest.warnings.is_empty() {
                value["warnings"] = json!(digest.warnings);
            }
            Ok(value)
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
                raw_message: input.optional_raw_string("raw_message")?,
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
