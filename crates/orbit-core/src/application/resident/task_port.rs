//! Resident's side of the task-owned resident port [ORB-22955].

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequest, Task};

use super::comments::ensure_resident_comment_redaction_safe;
use crate::OrbitRuntime;
use crate::application::task::{ResidentMandateAttachment, TaskResidentPort};

impl TaskResidentPort for OrbitRuntime {
    fn apply_resident_authority_decision_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        actor_label: &str,
    ) -> Result<(), OrbitError> {
        self.apply_resident_grant_answer(request, answer_id, actor_label)
    }

    fn admit_locked_resident_comment(
        &self,
        task: &Task,
        message: Option<&str>,
        human_writer: bool,
    ) -> Result<(), OrbitError> {
        self.validate_resident_comment_write(task, message, human_writer)
    }

    fn refuse_resident_comment_needing_redaction(&self, message: &str) -> Result<(), OrbitError> {
        ensure_resident_comment_redaction_safe(message)
    }

    fn resident_mandate_error(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(super::resident_mandate_from_artifact(self, task_id)?.err())
    }

    fn resident_crew_from_mandate(&self, content: &[u8]) -> Option<String> {
        self.check_resident_mandate(content)
            .ok()
            .map(|mandate| mandate.crew)
    }

    fn apply_task_mandate_attachment(
        &self,
        task_id: &str,
    ) -> (Option<Task>, ResidentMandateAttachment) {
        self.finish_resident_mandate_attachment(task_id)
    }
}
