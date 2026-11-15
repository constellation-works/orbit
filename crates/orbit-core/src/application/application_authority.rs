//! Built-in operations reached only after application consent admitted the call.

use crate::application::auto_tasks::crud::{self, AutoTaskAddParams, AutoTaskUpdateParams};
use crate::{OrbitRuntime, application::task::TaskUpdateParams};
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde::Deserialize;
use serde_json::Value;

impl OrbitRuntime {
    pub(crate) fn execute_application_builtin(
        &self,
        tool: &str,
        input: &Value,
        session: &ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError> {
        let Some(invocation) = &session.application else {
            return Ok(None);
        };
        self.check_tool_enabled(tool)?;
        let string = |key: &str| -> Result<Option<String>, OrbitError> {
            input
                .get(key)
                .map(|v| {
                    v.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "application operation `{key}` must be a string"
                        ))
                    })
                })
                .transpose()
        };
        let required = |key: &str| {
            string(key)?.ok_or_else(|| {
                OrbitError::InvalidInput(format!("application operation needs `{key}`"))
            })
        };
        let actor = format!("application:{}", invocation.consent.application_id);
        let provenance = format!(
            "application consent {} by {} ({})",
            invocation.consent.consent_id,
            invocation.consent.consented_by,
            invocation.consent.consent_provenance
        );
        match tool {
            "orbit.task.update" => {
                let status = string("status")?
                    .map(|status| {
                        status.parse::<TaskStatus>().map_err(|_| {
                            OrbitError::InvalidInput("invalid application task status".into())
                        })
                    })
                    .transpose()?;
                let note = string("note")?;
                if note.is_some() && status.is_none() {
                    return Err(OrbitError::InvalidInput(
                        "`note` requires a status change; use `comment` for free-form discussion"
                            .into(),
                    ));
                }
                let id = required("id")?;
                // A constrained entry read the task's status; its write
                // rechecks it under the task lock. Callbacks inherit the
                // entry's evidence but walk the status on, so they do not.
                let expected_status = invocation
                    .predicate
                    .as_ref()
                    .filter(|seen| !invocation.callback && seen.task == id)
                    .map(|seen| {
                        seen.status_seen.parse::<TaskStatus>().map_err(|_| {
                            OrbitError::InvalidInput("invalid application task status".into())
                        })
                    })
                    .transpose()?;
                let task = self.update_task_as_application(
                    &id,
                    TaskUpdateParams {
                        status,
                        comment: string("comment")?,
                        plan: string("plan")?,
                        execution_summary: string("execution_summary")?,
                        ..Default::default()
                    },
                    actor,
                    note,
                    expected_status,
                )?;
                Ok(Some(
                    serde_json::json!({"id": task.id, "status": task.status}),
                ))
            }
            "orbit.decision.answer" => {
                let request = self.answer_decision_request_as_human(
                    &required("id")?,
                    &required("answer")?,
                    string("note")?,
                    actor,
                    Some(provenance),
                )?;
                Ok(Some(crate::application::task::decision_request_json(
                    &request,
                )))
            }
            "orbit.auto_task.add" => {
                let parsed: ApplicationAutoTaskAdd = parse_application_input(input)?;
                // Admitted by the ceiling and already checked. Not stored.
                let _workspace = parsed.workspace;
                let warnings = self.validate_required_tools(&parsed.template.required_tools)?;
                let definition = {
                    let _actor = crud::record_write_actor(actor);
                    self.auto_task_add(AutoTaskAddParams {
                        name: parsed.name,
                        description: parsed.description,
                        schedule: parsed.schedule,
                        template: parsed.template,
                        dedupe: parsed.dedupe,
                    })?
                };
                Ok(Some(auto_task_output(&definition, warnings)?))
            }
            "orbit.auto_task.update" => {
                let parsed: ApplicationAutoTaskUpdate = parse_application_input(input)?;
                // Admitted by the ceiling and already checked. Not stored.
                let _workspace = parsed.workspace;
                let definition = {
                    let _actor = crud::record_write_actor(actor);
                    self.auto_task_update(
                        &parsed.name,
                        AutoTaskUpdateParams {
                            enabled: parsed.enabled,
                            ..AutoTaskUpdateParams::default()
                        },
                    )?
                };
                let warnings = self.auto_task_update_tool_warnings(&definition, false)?;
                let mut output = auto_task_output(&definition, warnings)?;
                output["effective_enabled"] =
                    serde_json::json!(self.auto_task_enabled(&definition));
                Ok(Some(output))
            }
            _ => Ok(None),
        }
    }
}

/// Admitted `orbit.auto_task.add` input. `workspace` is a ceiling field and
/// is not part of the definition. Any other key is refused before a write.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationAutoTaskAdd {
    name: String,
    #[serde(default)]
    description: String,
    schedule: AutoTaskSchedule,
    template: AutoTaskTemplate,
    #[serde(default)]
    dedupe: DedupePolicy,
    #[serde(default)]
    workspace: Option<Value>,
}

/// Admitted `orbit.auto_task.update` input. Only the approved name and
/// enabled state are applied. `workspace` is ignored. `expected_enabled`
/// and every other key are refused before a write.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplicationAutoTaskUpdate {
    name: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    workspace: Option<Value>,
}

fn parse_application_input<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, OrbitError> {
    serde_json::from_value(input.clone()).map_err(|_| {
        OrbitError::InvalidInput(
            "application auto-task input does not match the approved operation".into(),
        )
    })
}

fn auto_task_output(
    definition: &AutoTaskDefinition,
    warnings: Vec<String>,
) -> Result<Value, OrbitError> {
    let mut output = serde_json::to_value(definition)
        .map_err(|error| OrbitError::Io(format!("encode auto-task: {error}")))?;
    if !warnings.is_empty()
        && let Some(object) = output.as_object_mut()
    {
        object.insert("warnings".to_string(), serde_json::json!(warnings));
    }
    Ok(output)
}
