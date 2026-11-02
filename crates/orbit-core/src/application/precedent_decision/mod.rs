//! Shadow precedent-decision consumer. Default off. A sealed prediction is
//! private ledger state and is never copied onto a decision surface. An
//! operator-enabled category may also self-answer low-risk requests
//! (`assist`), under the host-only enablement store (`enablement`).
mod assist;
mod contract;
mod enablement;
mod export;
mod ledger;
mod runtime;

pub use contract::{PRECEDENT_DECISION_USE_CASE, precedent_decision_question_template};
pub use enablement::{
    PRECEDENT_ASSIST_DEMOTION_WINDOW, PRECEDENT_ASSIST_HOLDBACK_SHARE, PRECEDENT_ASSIST_MAX_VETOES,
    PRECEDENT_ASSIST_WAIT_SECONDS, PRECEDENT_HALF_LIFE_DAYS, PrecedentAssistDemotion,
    PrecedentAssistEnablement, PrecedentAssistEnd, PrecedentAssistOutcome,
    PrecedentAssistOutcomeSource, PrecedentAssistState, PrecedentAssistStatus,
};
pub use ledger::{PrecedentAssistRouting, PrecedentDecisionReceipt};
