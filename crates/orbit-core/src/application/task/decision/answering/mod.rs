//! Apply and validate a human decision answer.

mod admission;
#[cfg(feature = "test-support")]
pub(crate) mod answer_checkpoint;
mod options;
mod record;

use crate::application::task::TriageTarget;

use crate::application::task::transitions::DeferredApprovalEffects;

pub(in crate::application::task) use admission::{decision_comment, ensure_request_answerable};

/// What an answer's comment says when its rewrite discarded the task's
/// published or kept candidate, which the rewritten specification no longer
/// matches.
const CANDIDATE_DISCARDED_NOTE: &str = "\nCandidate discarded: the rewrite changed the specification the task's published candidate implements, so that candidate (and its open pull request) is closed and the next run implements fresh.";

#[derive(Default)]
struct DecisionAnswerEffects {
    deferred_approval: Option<DeferredApprovalEffects>,
    requirements_rewritten: bool,
}

/// One triage option's edit to the held task.
struct TriageEdit {
    target: TriageTarget,
    /// The option's replacement text as the proposal recorded it.
    replacement: String,
    /// `replacement` as a criteria list; what an acceptance-criteria target
    /// writes.
    criteria: Vec<String>,
}
