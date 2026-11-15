use orbit_common::OrbitError;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use orbit_tools::OrbitBuiltinAction;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};

use super::ArtifactRedactionReport;

pub(super) fn emit_audit_events(
    runtime: &OrbitRuntime,
    action: OrbitBuiltinAction,
    response: &Value,
    report: &ArtifactRedactionReport,
    persisted_task_id: Option<&str>,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), OrbitError> {
    let tool_name = tool_name(action);
    let artifact = artifact_target(action, response, persisted_task_id)?;
    let actor = normalize_optional_attribution_label(model.or(agent), model)
        .unwrap_or_else(|| runtime.actor_label().to_string());

    for field in &report.fields {
        let redaction_kinds = field
            .kinds
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>();
        let payload = json!({
            "artifact_type": artifact.artifact_type,
            "artifact_id": artifact.artifact_id,
            "field_path": field.field_path,
            "actor": actor,
            "tool_name": tool_name,
            "redaction_kinds": redaction_kinds,
            "redaction_classes": field.classes,
        });
        record_system_audit_event(
            runtime,
            SystemAuditEvent {
                execution_id_prefix: "audit-artifact-redaction",
                command: "artifact_redaction",
                subcommand: Some("field"),
                tool_name: Some(tool_name),
                target_type: artifact.artifact_type,
                target_id: Some(artifact.artifact_id),
                role: &actor,
                status: AuditEventStatus::Success,
                arguments_json: payload.to_string(),
                workspace_id: None,
                task_id: artifact.task_id,
                attribution: AuditRunAttribution::Environment { run_id: None },
            },
        )?;
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct ArtifactTarget<'a> {
    artifact_type: &'static str,
    artifact_id: &'a str,
    task_id: Option<&'a str>,
}

fn artifact_target<'a>(
    action: OrbitBuiltinAction,
    response: &'a Value,
    persisted_task_id: Option<&'a str>,
) -> Result<ArtifactTarget<'a>, OrbitError> {
    match action {
        OrbitBuiltinAction::Task(verb) => match verb {
            TaskVerb::Add
            | TaskVerb::Update
            | TaskVerb::ArtifactPut
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject => {
                let id = persisted_task_id.ok_or_else(|| {
                    OrbitError::Execution("redaction audit missing persisted task id".to_string())
                })?;
                Ok(ArtifactTarget {
                    artifact_type: "task",
                    artifact_id: id,
                    task_id: Some(id),
                })
            }
            TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Show
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::Delete => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
        // Every friction verb that redacts writes the record its response
        // names.
        OrbitBuiltinAction::Friction(
            FrictionVerb::Add
            | FrictionVerb::List
            | FrictionVerb::Show
            | FrictionVerb::Stats
            | FrictionVerb::Tags
            | FrictionVerb::Update
            | FrictionVerb::Resolve
            | FrictionVerb::Rehome,
        ) => Ok(ArtifactTarget {
            artifact_type: "friction",
            artifact_id: response_string(response, "id")?,
            task_id: None,
        }),
        OrbitBuiltinAction::AutoTask(verb) => match verb {
            AutoTaskVerb::Add | AutoTaskVerb::Update => Ok(ArtifactTarget {
                artifact_type: "auto_task",
                artifact_id: response_string(response, "name")?,
                task_id: None,
            }),
            AutoTaskVerb::List
            | AutoTaskVerb::Show
            | AutoTaskVerb::Toggle
            | AutoTaskVerb::Delete
            | AutoTaskVerb::Restore
            | AutoTaskVerb::Mint
            | AutoTaskVerb::Recover
            | AutoTaskVerb::Reset => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::ClaimEffort
            | DrainVerb::Claims => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
        _ => Err(OrbitError::Execution(format!(
            "unsupported redaction audit action: {action:?}"
        ))),
    }
}

fn response_string<'a>(response: &'a Value, field: &str) -> Result<&'a str, OrbitError> {
    response.get(field).and_then(Value::as_str).ok_or_else(|| {
        OrbitError::Execution(format!("redaction audit response missing string `{field}`"))
    })
}

fn tool_name(action: OrbitBuiltinAction) -> &'static str {
    match action {
        OrbitBuiltinAction::Task(verb) => verb.tool_name(),
        OrbitBuiltinAction::Decision(verb) => verb.tool_name(),
        OrbitBuiltinAction::Experiment(verb) => verb.tool_name(),
        OrbitBuiltinAction::Friction(verb) => verb.tool_name(),
        OrbitBuiltinAction::AutoTask(verb) => verb.tool_name(),
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::ClaimEffort
            | DrainVerb::Claims => verb.tool_name(),
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => verb.tool_name(),
        },
        _ => "orbit.unknown",
    }
}
