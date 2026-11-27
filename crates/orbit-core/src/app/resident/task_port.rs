//! Resident's side of the task-owned resident port [ORB-22955].

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequest, Task};

use super::assignment::finish_resident_mandate_attachment;
use super::comment::{ensure_resident_comment_redaction_safe, validate_resident_comment_write};
use super::grant::apply_resident_grant_answer;
use super::read::check_resident_mandate;
use crate::OrbitRuntime;
use crate::application::task::{ResidentMandateAttachment, TaskResidentPort};

/// Binds task's resident policy to the current workspace runtime.
pub(crate) struct TaskPort<'a>(pub(crate) &'a OrbitRuntime);

impl TaskResidentPort for TaskPort<'_> {
    fn apply_resident_authority_decision_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        actor_label: &str,
    ) -> Result<(), OrbitError> {
        apply_resident_grant_answer(self.0, request, answer_id, actor_label)
    }

    fn admit_locked_resident_comment(
        &self,
        task: &Task,
        message: Option<&str>,
        human_writer: bool,
    ) -> Result<(), OrbitError> {
        validate_resident_comment_write(self.0, task, message, human_writer)
    }

    fn refuse_resident_comment_needing_redaction(&self, message: &str) -> Result<(), OrbitError> {
        ensure_resident_comment_redaction_safe(message)
    }

    fn resident_mandate_error(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(super::resident_mandate_from_artifact(self.0, task_id)?.err())
    }

    fn resident_crew_from_mandate(&self, content: &[u8]) -> Option<String> {
        check_resident_mandate(self.0, content)
            .ok()
            .map(|mandate| mandate.crew)
    }

    fn apply_task_mandate_attachment(
        &self,
        task_id: &str,
    ) -> (Option<Task>, ResidentMandateAttachment) {
        finish_resident_mandate_attachment(self.0, task_id)
    }
}
