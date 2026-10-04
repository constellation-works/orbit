use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, DESKTOP_TASK_COMPLETE, DESKTOP_TASK_EDIT, authorize,
    governed_tool,
};
use orbit_types::{
    task::{Task, TaskStatus},
    tool::ToolSessionContext,
};

use crate::OrbitRuntime;
use crate::application::task::{TaskUpdateParams, lifecycle::ensure_status_change_allowed};

use super::validation::{criteria, invalid};

impl OrbitRuntime {
    fn desktop_tool_allowed(session: &ToolSessionContext, name: &str) -> Result<(), OrbitError> {
        let operation = governed_tool(name).ok_or_else(|| invalid("unknown governed operation"))?;
        authorize(
            operation,
            &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(session)),
        )
        .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))
    }
    pub(super) fn desktop_run_allowed(
        &self,
        task: &Task,
        session: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        Self::desktop_tool_allowed(session, "orbit.workflow.run.show")?;
        if task.job_run_id.is_none() {
            return Err(invalid("no execution run is linked"));
        }
        if let Some(host) = &task.job_run_machine
            && self.automation_machine_identity() != Some(host.machine_id.as_str())
        {
            return Err(invalid(&format!(
                "Execution on {}",
                host.machine_name.as_deref().unwrap_or(&host.machine_id)
            )));
        }
        Ok(())
    }
    pub(super) fn desktop_ship_allowed(
        &self,
        task: &Task,
        session: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        Self::desktop_tool_allowed(session, "orbit.workflow.ship")?;
        if task.status != TaskStatus::Backlog {
            return Err(invalid("shipment requires backlog"));
        }
        self.resolve_crew_for_task(None, task.crew.as_deref())?;
        let statuses = self.dependency_status_index([task])?;
        if !orbit_types::task::unmet_task_dependencies(task, &statuses).is_empty() {
            return Err(invalid("task dependencies are not complete"));
        }
        Ok(())
    }
    pub(super) fn desktop_status_allowed(
        &self,
        task: &Task,
        target: TaskStatus,
        session: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        authorize(
            &DESKTOP_TASK_EDIT,
            &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(session)),
        )
        .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))?;
        if target == TaskStatus::Done {
            return Err(invalid("use evidence-bound review to complete a task"));
        }
        if target == TaskStatus::InProgress {
            return Err(invalid("use Ship to start execution"));
        }
        ensure_status_change_allowed(self, task, &TaskUpdateParams::default(), target)
    }
    pub(super) fn desktop_completion_allowed(
        &self,
        task: &Task,
        session: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        authorize(
            &DESKTOP_TASK_COMPLETE,
            &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(session)),
        )
        .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))?;
        if task.status != TaskStatus::Review {
            return Err(invalid("desktop completion requires review state"));
        }
        criteria(&task.acceptance_criteria)?;
        let mut pending = task.job_run_id.clone().into_iter().collect::<Vec<_>>();
        let mut visited = std::collections::HashSet::new();
        while let Some(run_id) = pending.pop() {
            if !visited.insert(run_id.clone()) {
                continue;
            }
            let run = self.get_job_run_backend(&run_id)?.ok_or_else(|| {
                invalid(
                    "linked review run is unavailable; completion cannot verify stopped execution",
                )
            })?;
            if matches!(
                run.state,
                orbit_types::workflow::JobRunState::Pending
                    | orbit_types::workflow::JobRunState::Running
                    | orbit_types::workflow::JobRunState::Retrying
            ) {
                return Err(invalid(
                    "linked review run is not stopped; pending, running and retrying execution cannot be completed",
                ));
            }
            if let Some(state) = self.read_run_state(&run_id)? {
                pending.extend(
                    state
                        .child_dispatches
                        .into_iter()
                        .map(|child| child.child_run_id),
                );
            }
        }
        self.ensure_resolves_are_workspace_local(task)?;
        ensure_status_change_allowed(self, task, &TaskUpdateParams::default(), TaskStatus::Done)
    }
}
