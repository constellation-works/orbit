//! The operation-mode handler table [ORB-11332].
//!
//! Verbs are declared once in `orbit_common::governance::operation_mode`; the
//! handlers need `&OrbitRuntime`, so they live here and are joined to the
//! spec table by [`OperationModeVerb`]. The exhaustive `match` in
//! [`dispatch`] makes a spec without a handler a compile error.

use orbit_common::OrbitError;
use orbit_common::governance::operation_mode::OperationModeVerb;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_duration_seconds, optional_string,
    optional_u32_alias, required_string,
};
use orbit_config::{CompletionPreference, OperationLayer, OperationPreset, ReviewPolicy};
use orbit_types::workflow::{GrantRights, OperationGrant};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::operation::{EnableOperationGrantRequest, OperationGrantControlRequest};

const DEFAULT_LIST_LIMIT: u32 = 20;

/// Route one operation-mode verb to its handler.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: OperationModeVerb,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let actor = runtime
        .actor()
        .resolve_write_label(agent.as_deref(), model.as_deref())?;
    match verb {
        OperationModeVerb::Explain => explain(runtime, &input),
        OperationModeVerb::Enable => enable(runtime, &input, &actor),
        OperationModeVerb::List => list(runtime, &input),
        OperationModeVerb::Show => show(runtime, &input),
        OperationModeVerb::Stop => stop(runtime, &input, &actor),
        OperationModeVerb::Revoke => revoke(runtime, &input, &actor),
    }
}

/// The run-layer overrides a request may supply.
fn run_layer(input: &Value) -> Result<OperationLayer, OrbitError> {
    Ok(OperationLayer {
        preset: optional_string(input, "preset")?
            .map(|raw| OperationPreset::parse(&raw))
            .transpose()?,
        completion: optional_string(input, "completion")?
            .map(|raw| CompletionPreference::parse(&raw))
            .transpose()?,
        leaf_ceiling: optional_u32_alias(input, &["leaf_ceiling"])?,
        recovery_episodes_per_task: optional_u32_alias(input, &["recovery_episodes"])?,
        recovery_minutes_per_task: optional_u32_alias(input, &["recovery_minutes"])?,
        review_policy: optional_string(input, "review_policy")?
            .map(|raw| ReviewPolicy::parse(&raw))
            .transpose()?,
        ..OperationLayer::default()
    })
}

fn explain(runtime: &OrbitRuntime, input: &Value) -> Result<Value, OrbitError> {
    let layer = run_layer(input)?;
    runtime.explain_operation((!layer.is_empty()).then_some(&layer))
}

fn enable(runtime: &OrbitRuntime, input: &Value, actor: &str) -> Result<Value, OrbitError> {
    let task_ids =
        optional_csv_or_string_list_alias(input, &["task_ids", "task"])?.unwrap_or_default();
    let window = required_string(input, &["window", "for"], "window")?;
    let window_seconds =
        optional_duration_seconds(&json!({ "window": window }), "window")?.unwrap_or_default();
    let rights = GrantRights::parse(
        &optional_csv_or_string_list_alias(input, &["rights", "right"])?.unwrap_or_default(),
    )
    .map_err(OrbitError::InvalidInput)?;
    let claim_token = optional_string(input, "claim_token")?;
    let grant = runtime.enable_operation_grant(EnableOperationGrantRequest {
        task_ids: &task_ids,
        window_seconds,
        rights,
        run_layer: run_layer(input)?,
        actor,
        source: "tool",
        claim_token: claim_token.as_deref(),
    })?;
    grant_json(&grant)
}

fn list(runtime: &OrbitRuntime, input: &Value) -> Result<Value, OrbitError> {
    let limit = optional_u32_alias(input, &["limit"])?.unwrap_or(DEFAULT_LIST_LIMIT);
    let grants = runtime.list_operation_grants(limit as usize)?;
    grants
        .iter()
        .map(grant_json)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn show(runtime: &OrbitRuntime, input: &Value) -> Result<Value, OrbitError> {
    let id = required_string(input, &["id"], "id")?;
    grant_json(&runtime.operation_grant(&id)?)
}

fn control<'a>(
    input: &'a Value,
    actor: &'a str,
    id: &'a Option<String>,
    reason: &'a Option<String>,
    claim_token: &'a Option<String>,
) -> Result<OperationGrantControlRequest<'a>, OrbitError> {
    Ok(OperationGrantControlRequest {
        grant_id: id.as_deref(),
        reason: reason.as_deref(),
        expected_revision: optional_u32_alias(input, &["if_revision"])?,
        actor,
        source: "tool",
        claim_token: claim_token.as_deref(),
    })
}

fn stop(runtime: &OrbitRuntime, input: &Value, actor: &str) -> Result<Value, OrbitError> {
    let id = optional_string(input, "id")?;
    let reason = optional_string(input, "reason")?;
    let claim_token = optional_string(input, "claim_token")?;
    let result =
        runtime.stop_operation_grant(control(input, actor, &id, &reason, &claim_token)?)?;
    control_json(result.outcome, &result.grant, &result.coordinators)
}

fn revoke(runtime: &OrbitRuntime, input: &Value, actor: &str) -> Result<Value, OrbitError> {
    let id = optional_string(input, "id")?;
    let reason = optional_string(input, "reason")?;
    let claim_token = optional_string(input, "claim_token")?;
    let result =
        runtime.revoke_operation_grant(control(input, actor, &id, &reason, &claim_token)?)?;
    control_json(result.outcome, &result.grant, &result.coordinators)
}

/// The grant projection every surface renders: the durable record plus the
/// derived admission answer at read time.
pub(crate) fn grant_json(grant: &OperationGrant) -> Result<Value, OrbitError> {
    let now = chrono::Utc::now();
    let mut value = serde_json::to_value(grant)
        .map_err(|error| OrbitError::Execution(format!("serialize operation grant: {error}")))?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "admission".to_string(),
            json!(grant.admission(now).reason()),
        );
        object.insert(
            "remaining_seconds".to_string(),
            json!(grant.remaining_seconds(now)),
        );
        object.insert("rights_granted".to_string(), json!(grant.rights.names()));
    }
    Ok(value)
}

fn control_json(
    outcome: &str,
    grant: &OperationGrant,
    coordinators: &[crate::application::job::DrainAdmissionsStopChange],
) -> Result<Value, OrbitError> {
    let mut value = grant_json(grant)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("outcome".to_string(), json!(outcome));
        object.insert(
            "coordinators".to_string(),
            json!(
                coordinators
                    .iter()
                    .map(|change| json!({
                        "run_id": change.run_id,
                        "outcome": change.outcome,
                        "remaining_children": change.remaining_children.len(),
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(value)
}
