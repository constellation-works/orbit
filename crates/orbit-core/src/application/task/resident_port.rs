//! What task writes and decision answers need from resident policy
//! [ORB-22955].
//!
//! A `resident_authority` decision answer applies a human's grant, and every
//! task comment that carries a resident record is admitted against its
//! mandate and refused when its verbatim text would need redaction.
//! `resident` owns that policy and also drives task writes, so a direct call
//! from here would close a cycle. This port is the way in: `resident`
//! implements it on [`OrbitRuntime`](crate::OrbitRuntime), so composing the
//! runtime binds it.

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequest, Task};

/// The resident-policy operations task code calls, implemented by `resident`.
pub(in crate::application) trait TaskResidentPort {
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
}
