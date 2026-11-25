use super::super::super::dispatch::ToolCaller;
use super::super::super::input::parse_task_status;
use super::super::super::json::serialize_error;
use super::super::write::TaskWriteOutput;
use super::lifecycle::{block_triage, delete, lint, reject};
use super::listing::{eligible, list};
use super::show::show;
use crate::OrbitRuntime;
use crate::application::resident::ResidentActionTicket;
use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::task::TaskVerb;
use serde_json::Value;

/// Run one migrated task verb. A write also answers the id it persisted, for
/// the redaction audit. A resident action key comes from the authenticated
/// broker and records that the live cycle held the right this write needs.
pub(in crate::adapter::tool_host) fn dispatch(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    operator: bool,
    resident_action_key: Option<&str>,
    resident_ticket: Option<&ResidentActionTicket>,
    verb: TaskVerb,
    input: Value,
) -> Result<(Value, Option<String>), OrbitError> {
    // The attachment surface has already authenticated/prepared its path-free
    // payload. Its public source_path parameter is no longer present here.
    if verb == TaskVerb::ArtifactPut {
        let output = super::super::write::artifact_put(runtime, caller, &input, operator)?;
        return Ok((output.response, Some(output.persisted_id)));
    }
    // Re-wraps the input the tool entry already parsed; see `parse_input`.
    let input = verb.spec().parse_input(input)?;
    let resident_cycle_admitted = resident_action_key.is_some();
    let read = |value: Result<Value, OrbitError>| value.map(|value| (value, None));
    let written = |output: Result<TaskWriteOutput, OrbitError>| {
        output.map(|output| (output.response, Some(output.persisted_id)))
    };
    match verb {
        TaskVerb::Add => written(super::super::write::add(runtime, caller, input.as_value())),
        TaskVerb::Update => written(super::super::write::update(
            runtime,
            caller,
            input.as_value(),
            operator,
            resident_ticket,
        )),
        TaskVerb::ArtifactPut => Err(OrbitError::Execution(
            "artifact preparation dispatch invariant violated".into(),
        )),
        TaskVerb::ArtifactGet => read(super::super::read::artifact_get(
            runtime,
            caller.session_context,
            &input,
        )),
        TaskVerb::LocksList => read(crate::runtime::task::locks::list(runtime)),
        TaskVerb::LocksReserve => read(crate::runtime::task::locks::reserve(
            runtime,
            input.into_value(),
            caller.agent,
            caller.model,
            caller.reservation_owner,
        )),
        TaskVerb::LocksRelease => read(crate::runtime::task::locks::release(
            runtime,
            input.into_value(),
            caller.agent,
            caller.model,
        )),
        TaskVerb::List => read(list(runtime, &input)),
        TaskVerb::Eligible => read(eligible(runtime, &input)),
        TaskVerb::Show => read(show(runtime, &input)),
        TaskVerb::Lint => read(lint(runtime, &input)),
        TaskVerb::ReviewReset => reviewed(crate::application::review::reset_review(
            runtime,
            input.as_value(),
        )),
        TaskVerb::Repilot => {
            if !resident_cycle_admitted {
                runtime.admit_operator_operation("orbit.task.repilot", caller.session_context)?;
            }
            let id = input.required_string("id")?;
            let trigger = if caller.session_context.transport.is_some() {
                orbit_types::workflow::JobRunTrigger::mcp()
            } else {
                orbit_types::workflow::JobRunTrigger::cli()
            };
            let full = input
                .as_value()
                .get("full")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let submission = runtime
                .submit_explicit_task_pilot_in_mode(&id, caller.agent.as_deref(), trigger, full)
                .map_err(OrbitError::from)?;
            Ok((
                serde_json::to_value(submission)
                    .map_err(serialize_error("serialize pilot submission"))?,
                Some(id),
            ))
        }
        TaskVerb::Requeue => {
            // The tool chokepoint admits only an operator or a resident cycle;
            // any other managed agent is refused here as well, including an
            // accidentally elevated run.
            if !resident_cycle_admitted
                && orbit_common::governance::authorization::agent_context_declared(
                    runtime.execution_env_policy().process_env(),
                )
            {
                return Err(OrbitError::CapabilityDenied(
                    "managed agents cannot requeue blocked tasks".into(),
                ));
            }
            if resident_cycle_admitted
                && input
                    .as_value()
                    .get("discard_candidate")
                    .and_then(Value::as_bool)
                    == Some(true)
            {
                return Err(OrbitError::CapabilityDenied(
                    "resident requeues cannot set `discard_candidate`".into(),
                ));
            }
            let id = input.required_string("id")?;
            let reason = input.required_string("reason")?;
            let discard = input
                .as_value()
                .get("discard_candidate")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let response = if resident_cycle_admitted {
                // The trusted identity of the cycle's run, never the host's
                // process actor, which may resolve to a person.
                let family = orbit_types::identity::require_canonical_agent_family(
                    caller.agent.as_deref(),
                    caller.model.as_deref(),
                )?
                .ok_or_else(|| {
                    OrbitError::CapabilityDenied(
                        "a resident requeue needs the cycle run's agent identity".into(),
                    )
                })?;
                runtime.requeue_blocked_task_as_resident(&id, &reason, discard, &family)?
            } else {
                runtime.requeue_blocked_task(&id, &reason, discard)?
            };
            Ok((response, Some(id)))
        }
        TaskVerb::ResumeDelivery => {
            let key = resident_action_key.ok_or_else(|| {
                OrbitError::CapabilityDenied(
                    "delivery recovery requires an authenticated resident cycle".into(),
                )
            })?;
            let id = input.required_string("id")?;
            let run_id = input.required_string("run_id")?;
            let diagnosis = input.required_string("diagnosis")?;
            let actor = orbit_types::identity::require_canonical_agent_family(
                caller.agent.as_deref(),
                caller.model.as_deref(),
            )?
            .ok_or_else(|| {
                OrbitError::CapabilityDenied(
                    "delivery recovery requires the cycle run's agent identity".into(),
                )
            })?;
            let response =
                runtime.submit_resident_delivery_recovery(&id, &run_id, &diagnosis, key, &actor)?;
            Ok((response, Some(id)))
        }
        TaskVerb::BlockTriage => block_triage(runtime, caller, &input),
        TaskVerb::ReconcileReview => reviewed(
            crate::application::review::reconciliation::reconcile_review(
                runtime,
                caller.session_context,
                input.as_value(),
            ),
        ),
        TaskVerb::RetryHandoff => {
            if orbit_common::governance::authorization::agent_context_declared(
                runtime.execution_env_policy().process_env(),
            ) {
                return Err(OrbitError::CapabilityDenied(
                    "managed agents cannot retry owner handoffs".into(),
                ));
            }
            let authority = runtime
                .admit_operator_operation("orbit.task.retry_handoff", caller.session_context)?;
            let actor = authority
                .remote_caller_machine_id
                .map_or_else(|| runtime.actor().resolve_write_label(None, None), Ok)?;
            let workspace = input.required_string("workspace")?;
            let owner = runtime.workspace_id()?;
            // The host adapter has selected the runtime. Direct runtime tool
            // callers must also explicitly name this partition or checkout.
            if workspace != owner
                && std::path::Path::new(&workspace)
                    .canonicalize()
                    .ok()
                    .as_deref()
                    != Some(runtime.paths().repo_root.as_path())
            {
                return Err(OrbitError::CapabilityDenied(
                    "retry must name this owner workspace explicitly".into(),
                ));
            }
            let mut request = input.into_value();
            if let Some(object) = request.as_object_mut() {
                object.remove("workspace");
            }
            let request = serde_json::from_value(request).map_err(|error| {
                OrbitError::InvalidInput(format!("invalid retry request: {error}"))
            })?;
            let dispatch = runtime.retry_handoff_as_operator(&owner, &request, &actor)?;
            let response = serde_json::to_value(&dispatch)
                .map_err(serialize_error("serialize landing retry"))?;
            Ok((response, Some(dispatch.task_id)))
        }
        TaskVerb::RecoverClaim => {
            if orbit_common::governance::authorization::agent_context_declared(
                runtime.execution_env_policy().process_env(),
            ) {
                return Err(OrbitError::CapabilityDenied(
                    "managed agents cannot recover claims".into(),
                ));
            }
            let authority = runtime
                .admit_operator_operation("orbit.task.recover_claim", caller.session_context)?;
            let actor = authority
                .remote_caller_machine_id
                .map_or_else(|| runtime.actor().resolve_write_label(None, None), Ok)?;
            let claim_id = input.required_string("claim_id")?;
            let expected_phase = input.required_string("expected_phase")?;
            let status_raw = input.required_string("status")?;
            let status = parse_task_status("status", &status_raw)?;
            let reason = input.required_string("reason")?;
            let request_key = input.required_string("request_key")?;
            let response = runtime.recover_claim_as_operator(
                &claim_id,
                &expected_phase,
                status,
                &actor,
                &reason,
                ("operator-recover", &request_key),
            )?;
            // The response names the claim, not its task; resolve the task for
            // the redaction audit from the claim console's task id if present.
            let persisted = response
                .get("task_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| Some(claim_id.clone()));
            Ok((response, persisted))
        }
        TaskVerb::RevokeHandoff => {
            if orbit_common::governance::authorization::agent_context_declared(
                runtime.execution_env_policy().process_env(),
            ) {
                return Err(OrbitError::CapabilityDenied(
                    "managed agents cannot revoke handoff authority".into(),
                ));
            }
            let authority = runtime
                .admit_operator_operation("orbit.task.revoke_handoff", caller.session_context)?;
            let actor = authority
                .remote_caller_machine_id
                .map_or_else(|| runtime.actor().resolve_write_label(None, None), Ok)?;
            let handoff_id = input.required_string("handoff_id")?;
            let expected = crate::application::review::ExpectedCandidate {
                candidate_commit: input.required_string("expected_candidate_commit")?,
                base_commit: input.required_string("expected_base_commit")?,
            };
            let reason = input.required_string("reason")?;
            let request_key = input.required_string("request_key")?;
            let response = runtime.revoke_handoff_as_operator(
                &handoff_id,
                &expected,
                &actor,
                &reason,
                "operator-revoke",
                &request_key,
            )?;
            let persisted = response
                .get("claim_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| Some(handoff_id.clone()));
            Ok((response, persisted))
        }
        TaskVerb::Reject => written(reject(runtime, &input, caller.agent, caller.model)),
        TaskVerb::Delete => read(delete(runtime, &input)),
    }
}

/// A review verb's answer, and the task it names, for the redaction audit.
fn reviewed(value: Result<Value, OrbitError>) -> Result<(Value, Option<String>), OrbitError> {
    let value = value?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Ok((value, id))
}
