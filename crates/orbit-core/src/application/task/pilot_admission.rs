//! Backlog holds from applied pilot findings, including assessments made
//! without lifecycle promotion. Decisions are scoped to the latest assessment.

use orbit_common::OrbitError;
use serde_json::Value;

use super::helpers::is_automation_actor;
use crate::OrbitRuntime;

/// A finding in the latest applied assessment that requires an operator decision.
pub(crate) enum PilotAdmissionHold {
    Duplicate,
    AlreadyLanded,
}

impl OrbitRuntime {
    pub(crate) fn pilot_admission_hold(
        &self,
        task_id: &str,
    ) -> Result<Option<PilotAdmissionHold>, OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        // Atomic pilot application persists the full assessment after its replay
        // receipt in a task-pilot-authored comment. Read that durable format, not
        // the human-readable history summary or an uncommitted agent result.
        for comment in comments.iter().rev() {
            let mut lines = comment.message.lines();
            let header = lines.next().unwrap_or_default().trim();
            if !comment.by.trim().is_empty()
                && !is_automation_actor(&comment.by)
                && matches!(
                    header,
                    "task-pilot-admission: approve-anyway" | "task-pilot-admission: clear"
                )
            {
                return Ok(None);
            }
            if comment.by != "task-pilot" || !header.starts_with("operation_id=") {
                continue;
            }
            let (_, audit) = comment.message.split_once('\n').ok_or_else(|| {
                OrbitError::Execution("pilot receipt is missing its assessment".into())
            })?;
            let audit: Value = serde_json::from_str(audit).map_err(|error| {
                OrbitError::Execution(format!("decode pilot assessment: {error}"))
            })?;
            let assessment = audit.get("assessment").ok_or_else(|| {
                OrbitError::Execution("pilot receipt is missing its assessment".into())
            })?;
            for (field, hold) in [
                ("already_landed", PilotAdmissionHold::AlreadyLanded),
                ("duplicate_of", PilotAdmissionHold::Duplicate),
            ] {
                if assessment.get(field).is_some_and(|value| !value.is_null()) {
                    return Ok(Some(hold));
                }
            }
            // A clear latest assessment supersedes any older finding. Unrelated
            // comments and metadata edits do not release an existing hold.
            return Ok(None);
        }
        Ok(None)
    }
}
