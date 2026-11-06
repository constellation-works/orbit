//! The precedent consumer's side of the task-owned port [ORB-22313].

use orbit_types::task::DecisionRequest;

use crate::OrbitRuntime;
use crate::application::contracts::PrecedentAssistEnablement;
use crate::application::task::TaskPrecedentDecisionPort;

impl TaskPrecedentDecisionPort for OrbitRuntime {
    fn enqueue_open_decision_for_precedent(&self, request: &DecisionRequest) {
        self.note_open_precedent_decision(request);
    }

    fn record_model_answer_review_outcome(&self, original: &DecisionRequest, vetoed: bool) {
        self.note_precedent_assist_review(original, vetoed);
    }

    fn demoted_precedent_categories(&self) -> Vec<PrecedentAssistEnablement> {
        self.precedent_assist_demotions()
    }
}
