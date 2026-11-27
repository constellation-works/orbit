//! What task writes and decision answers need from resident policy
//! [ORB-22955].
//!
//! A `resident_authority` decision answer applies a human's grant, and every
//! task comment that carries a resident record is admitted against its
//! mandate and refused when its verbatim text would need redaction.
//! `resident` owns that policy and also drives task writes, so a direct call
//! from here would close a cycle. This port is the way in: `resident`
//! implements it on an adapter holding the runtime, bound in `app::port`.

use std::fmt;

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequest, Task};
use serde_json::{Value, json};

/// Cycle-routine status after a resident mandate was written.
#[derive(Debug, Clone)]
pub(crate) struct ResidentMandateAttachment {
    pub(crate) disabled_routine: Option<DisabledCycleRoutine>,
}

#[derive(Debug, Clone)]
pub(crate) struct DisabledCycleRoutine {
    pub(crate) name: String,
    pub(crate) reason: String,
}

impl ResidentMandateAttachment {
    pub(crate) fn resident_task_warning(&self, task_id: &str) -> Option<String> {
        self.warning_after(format_args!("Mandate {task_id} stored"))
    }

    pub(crate) fn insert_artifact_put_fields(&self, response: &mut Value) {
        self.insert_cycle_routine_fields(response, format_args!("Mandate stored"));
    }

    pub(crate) fn insert_cycle_routine_fields(
        &self,
        response: &mut Value,
        action: fmt::Arguments<'_>,
    ) {
        let Some(fields) = response.as_object_mut() else {
            return;
        };
        let disabled = self.disabled_routine.as_ref();
        fields.insert("cycle_routine_disabled".into(), json!(disabled.is_some()));
        fields.insert(
            "cycle_routine_name".into(),
            json!(disabled.map(|routine| &routine.name)),
        );
        if let Some(warning) = self.warning_after(action) {
            fields.insert("warning".into(), Value::String(warning));
        }
    }

    fn warning_after(&self, action: fmt::Arguments<'_>) -> Option<String> {
        let routine = self.disabled_routine.as_ref()?;
        Some(format!(
            "{action}, but {}. Enable the routine to let it act.",
            routine.reason
        ))
    }
}

/// The resident-policy operations task code calls, implemented by `resident`.
pub(crate) trait TaskResidentPort {
    /// Apply an allowed answer to a `resident_authority` decision `request`
    /// as the human `actor_label`. The caller holds the task's write lock
    /// and has refused stale and disallowed answers.
    fn apply_resident_authority_decision_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        actor_label: &str,
    ) -> Result<(), OrbitError>;

    /// Admit `message`, when it is a resident record, as a comment on
    /// `task`; `human_writer` is the trusted human-writer calculation. Called
    /// under the task lock, before any part of the update writes.
    fn admit_locked_resident_comment(
        &self,
        task: &Task,
        message: Option<&str>,
        human_writer: bool,
    ) -> Result<(), OrbitError>;

    /// Refuse a resident record whose verbatim `message` would need
    /// redaction, before any writer sanitizes it.
    fn refuse_resident_comment_needing_redaction(&self, message: &str) -> Result<(), OrbitError>;

    /// Explain why `task_id`'s `resident/mandate.json` artifact is missing or
    /// invalid, or `None` when a valid mandate is attached.
    fn resident_mandate_error(&self, task_id: &str) -> Result<Option<String>, OrbitError>;

    /// The crew of a valid supplied mandate, if the contents pass resident validation.
    fn resident_crew_from_mandate(&self, content: &[u8]) -> Option<String>;

    /// Activate an eligible assignment after storage and report the cycle-routine status.
    fn apply_task_mandate_attachment(
        &self,
        task_id: &str,
    ) -> (Option<Task>, ResidentMandateAttachment);
}
