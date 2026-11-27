//! The dashboard's resident reads and writes. Each calls the runtime method
//! or resident tool the dashboard's handler called before.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::DASHBOARD_RESIDENT_WRITE;
use orbit_common::governance::resident::ResidentVerb;
use orbit_types::task::TaskComplexity;
use serde_json::{Map, Value, json};

use super::{Cards, CycleRoutineScope, DashboardWrite, DashboardWritten};
use crate::app::exec::{DashboardOrigin, DashboardRow, Operation};
use crate::app::tool_call::{self, Input};
use crate::application::resident::{
    ResidentDashboardGrant, ResidentDashboardGrantOutcome, ResidentMandateCreateParams,
};
use crate::{Core, OrbitRuntime};

pub(super) fn cards(runtime: &OrbitRuntime, include_ended: bool) -> Result<Cards, OrbitError> {
    let mandates = runtime.list_resident_mandate_cards(include_ended)?;
    let crews =
        serde_json::to_value(runtime.configured_crew_registry_projection()).unwrap_or(Value::Null);
    // The selector is the logical catalog ID the dashboard addressed. The
    // runtime's `workspace_id()` is the persisted partition ID, which an
    // ordinary checkout may name differently.
    let cycle_routine = runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .or_else(|| runtime.workspace_id().ok())
        .map(|workspace_id| CycleRoutineScope {
            workspace_id,
            orbit_dir: runtime.paths().orbit_dir.clone(),
        });
    Ok(Cards {
        workspace: runtime.workspace_label(),
        mandates,
        crews,
        cycle_routine,
    })
}

/// One mandate as the resident show tool reports it to this session, with
/// the two card reads attached.
pub(super) fn card(core: &Core, task_id: String, operator: bool) -> Result<Value, OrbitError> {
    let mut fields = Map::new();
    fields.insert("task_id".into(), json!(task_id));
    let runtime = core.runtime();
    let mut shown = tool_call::through_tool_entry(
        runtime,
        ResidentVerb::Show,
        Input::from_dashboard(fields, operator),
    )?;
    runtime.attach_resident_card_fields(&mut shown);
    Ok(shown)
}

/// The governed operation one write requires and the audit row it declares.
pub(super) fn operation(origin: &DashboardOrigin, write: &DashboardWrite) -> Operation {
    let (recorded_as, target, arguments) = match write {
        DashboardWrite::Create { title, mandate, .. } => (
            "resident.create",
            "new".to_string(),
            json!({"title": title, "mandate": mandate}),
        ),
        DashboardWrite::Grant(params) => {
            let mut arguments = Map::new();
            arguments.insert("task_id".into(), json!(params.task_id));
            for (key, list) in [
                ("add", &params.add),
                ("workspaces", &params.workspaces),
                ("remove", &params.remove),
                ("drop_workspaces", &params.drop_workspaces),
            ] {
                if !list.is_empty() {
                    arguments.insert(key.into(), json!(list));
                }
            }
            for (key, value) in [("deadline", &params.deadline), ("quote", &params.quote)] {
                if let Some(value) = value
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    arguments.insert(key.into(), json!(value));
                }
            }
            (
                if params.widens() {
                    "resident.grant"
                } else {
                    "resident.narrow"
                },
                params.task_id.clone(),
                Value::Object(arguments),
            )
        }
        DashboardWrite::Control(fields) => (
            "resident.control",
            target_task(fields),
            Value::Object(fields.clone()),
        ),
        DashboardWrite::Relay(fields) => (
            "resident.relay",
            target_task(fields),
            Value::Object(fields.clone()),
        ),
    };
    Operation::dashboard_read("resident", "dashboard")
        .requires(&DASHBOARD_RESIDENT_WRITE)
        .dashboard_row(&DASHBOARD_RESIDENT_WRITE, target, arguments, origin.clone())
        .dashboard_row_recorded_as(recorded_as)
}

/// Run one admitted write.
pub(super) fn write(core: &Core, write: DashboardWrite) -> Result<DashboardWritten, OrbitError> {
    let runtime = core.runtime();
    match write {
        DashboardWrite::Create {
            title,
            description,
            complexity,
            mandate,
        } => {
            if !mandate.is_object() {
                return Err(OrbitError::InvalidInput(
                    "mandate must be a resident.mandate/v1 object".to_string(),
                ));
            }
            let complexity = complexity
                .as_deref()
                .map(str::trim)
                .filter(|complexity| !complexity.is_empty())
                .unwrap_or("medium")
                .parse::<TaskComplexity>()
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            runtime
                .create_resident_mandate(ResidentMandateCreateParams {
                    title,
                    description,
                    complexity,
                    mandate,
                })
                .map(DashboardWritten::Value)
        }
        DashboardWrite::Grant(params) => runtime
            .grant_resident_authority_as_dashboard_operator(params)
            .map(|grant| DashboardWritten::Granted(Box::new(grant))),
        DashboardWrite::Control(fields) => {
            channel(core, ResidentVerb::Control, fields).map(DashboardWritten::Value)
        }
        DashboardWrite::Relay(fields) => {
            channel(core, ResidentVerb::Relay, fields).map(DashboardWritten::Value)
        }
    }
}

/// Any failure is recorded as `refused`; a grant whose narrowing was refused
/// is recorded as a failure naming which step applied.
pub(super) fn row(
    result: Result<&DashboardWritten, &OrbitError>,
    arguments: Value,
) -> Option<DashboardRow> {
    Some(DashboardRow {
        failure: match result {
            Err(_) => Some("refused".to_string()),
            Ok(DashboardWritten::Granted(grant)) => grant_partial_failure(grant),
            Ok(DashboardWritten::Value(_)) => None,
        },
        target: None,
        arguments,
    })
}

/// The manager channel reaches its tool as the operator session the
/// dashboard operation just admitted.
fn channel(
    core: &Core,
    verb: ResidentVerb,
    fields: Map<String, Value>,
) -> Result<Value, OrbitError> {
    tool_call::through_tool_entry(core.runtime(), verb, Input::from_dashboard(fields, true))
}

fn target_task(fields: &Map<String, Value>) -> String {
    fields
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The audit failure of a grant whose widening outcome stands while its
/// narrowing was refused.
fn grant_partial_failure(grant: &ResidentDashboardGrant) -> Option<String> {
    let error = grant.narrowing_error.as_deref().unwrap_or_default();
    match grant.outcome {
        ResidentDashboardGrantOutcome::GrantedNarrowingRefused => {
            Some(format!("Widening applied; narrowing refused: {error}"))
        }
        ResidentDashboardGrantOutcome::UnchangedNarrowingRefused => {
            Some(format!("No widening applied; narrowing refused: {error}"))
        }
        ResidentDashboardGrantOutcome::Granted
        | ResidentDashboardGrantOutcome::Unchanged
        | ResidentDashboardGrantOutcome::Narrowed
        | ResidentDashboardGrantOutcome::GrantedAndNarrowed
        | ResidentDashboardGrantOutcome::UnchangedAndNarrowed => None,
    }
}
