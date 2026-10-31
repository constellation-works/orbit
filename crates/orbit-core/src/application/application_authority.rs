//! Built-in completion operations reached only after exact application consent.

use crate::{OrbitRuntime, application::task::TaskUpdateParams};
use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
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
                let task = self.update_task_as_human(
                    &required("id")?,
                    TaskUpdateParams {
                        status: string("status")?
                            .map(|status| {
                                status.parse().map_err(|_| {
                                    OrbitError::InvalidInput(
                                        "invalid application task status".into(),
                                    )
                                })
                            })
                            .transpose()?,
                        comment: string("comment")?,
                        ..Default::default()
                    },
                    actor,
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
            _ => Ok(None),
        }
    }
}
