//! What task writes need from the workspace agent corpus [ORB-23421].
//!
//! The corpus sits above `task`: it reads decision rules, task history and
//! tasks back, so a direct call from here would close a cycle. This port is
//! the way in: the corpus module implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.
//! Task keeps owning its decision-answer, precedent and final-recovery
//! writes; the corpus only proposes the history entries those writes carry.

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequest, DecisionResolution, TaskHistoryEntry};
use orbit_types::workflow::TransientInfraBlock;

use super::decision_rules::{CorpusDigest, CorpusRuleDependency};

/// The corpus reads and append proposals task code calls, implemented by
/// the corpus module above `task`.
pub(in crate::application) trait TaskCorpusPort {
    /// The corpus history entry a human decision answer carries in the same
    /// task-history update, when the answer earns one. Implemented by the
    /// corpus module.
    fn task_corpus_append_for_decision(
        &self,
        request: &DecisionRequest,
        resolution: &DecisionResolution,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError>;

    /// The current digest of one decision-rule dependency at `revision`.
    /// Implemented by the corpus module.
    fn task_corpus_digest_at_revision(
        &self,
        revision: &str,
        dependency: &CorpusRuleDependency,
    ) -> CorpusDigest;

    /// The corpus history entries a final-recovery escalation carries,
    /// proposed before the recovery mutation appends its history.
    /// Implemented by the corpus module.
    fn task_corpus_appends_for_recovery(
        &self,
        task_id: &str,
        infra: &TransientInfraBlock,
        outcome: &str,
    ) -> Result<Vec<TaskHistoryEntry>, OrbitError>;
}
