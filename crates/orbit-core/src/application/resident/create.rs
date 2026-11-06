//! Setting up a mandate in one action: the `resident`-tagged assignment and
//! its `resident/mandate.json` artifact.
//!
//! The terminal path is `orbit task add --tag resident` followed by
//! `orbit task artifact put … --path resident/mandate.json`. An in-process
//! surface (the dashboard) cannot hand a file to the artifact tool, so it
//! comes here instead: the draft is judged first by
//! [`OrbitRuntime::check_resident_mandate`], the rule every mandate read
//! applies, and nothing is written when it fails. The authority starts
//! empty; widening stays on `orbit.resident.grant`. The assignment leaves
//! here `in-progress`, as an attached mandate leaves the terminal path.

use orbit_common::OrbitError;
use orbit_types::task::{TaskArtifact, TaskComplexity, TaskOrigin};
use orbit_types::workflow::{RESIDENT_MANDATE_ARTIFACT, RESIDENT_TAG};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskRecordUpdateParams, TaskRecords};

/// One mandate to set up.
#[derive(Debug, Clone)]
pub struct ResidentMandateCreateParams {
    /// The assignment task's title.
    pub title: String,
    /// The assignment's narrative; the mandate's `goal` stays one line.
    pub description: String,
    /// The assignment's assessed complexity.
    pub complexity: TaskComplexity,
    /// The `resident.mandate/v1` document, as the artifact will hold it.
    pub mandate: Value,
}

impl OrbitRuntime {
    /// Create a mandate's assignment and attach its artifact. A draft the
    /// mandate rules refuse fails with their message and writes nothing.
    /// Returns `{task_id, status, mandate, cycle_routine_disabled,
    /// cycle_routine_name}`, plus a `warning` naming the disabled cycle
    /// routine when this workspace's routine is disabled.
    pub fn create_resident_mandate(
        &self,
        params: ResidentMandateCreateParams,
    ) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let bytes = serde_json::to_vec(&params.mandate)
            .map_err(|error| OrbitError::InvalidInput(format!("mandate: {error}")))?;
        let mandate = self
            .check_resident_mandate(&bytes)
            .map_err(OrbitError::InvalidInput)?;
        params.complexity.require_assessed()?;
        let content = serde_json::to_vec_pretty(&mandate)
            .map_err(|error| OrbitError::Execution(format!("serialize mandate: {error}")))?;
        let task = self.add_task(TaskAddParams {
            title: params.title,
            description: params.description,
            acceptance_criteria: mandate.exit_criteria.clone(),
            crew: Some(mandate.crew.clone()),
            tags: vec![RESIDENT_TAG.to_string()],
            complexity: params.complexity,
            origin: Some(TaskOrigin::OperatorRequest),
            ..TaskAddParams::default()
        })?;
        let actor = self.actor().resolve_write_label(None, None)?;
        self.stores()
            .task_records()
            .update(
                &task.id,
                TaskRecordUpdateParams {
                    actor,
                    upsert_artifacts: vec![TaskArtifact {
                        path: RESIDENT_MANDATE_ARTIFACT.to_string(),
                        content,
                        media_type: "application/json".to_string(),
                        created_by: None,
                    }],
                    ..Default::default()
                },
            )
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "task {} was created but {RESIDENT_MANDATE_ARTIFACT} was not attached: {error}; attach it with `orbit task artifact put`",
                    task.id
                ))
            })?;
        let status = self
            .activate_resident_assignment(&task.id)
            .map_or(task.status, |activated| activated.status);
        let cycle_routine_disabled = self.resident_cycle_routine_disabled();
        let mut result = json!({"task_id": task.id, "status": status, "mandate": mandate});
        result["cycle_routine_disabled"] = json!(cycle_routine_disabled.is_some());
        result["cycle_routine_name"] = json!(cycle_routine_disabled);
        if let Some(routine_name) = &cycle_routine_disabled {
            result["warning"] = json!(format!(
                "Created mandate {}, but {}. Enable the routine to let it act.",
                task.id,
                super::resident_cycle_routine_disabled_reason(routine_name)
            ));
        }
        Ok(result)
    }
}
