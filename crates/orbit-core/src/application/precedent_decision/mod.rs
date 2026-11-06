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
mod task_port;

pub use contract::{PRECEDENT_DECISION_USE_CASE, precedent_decision_question_template};
// The records' public paths here predate their move to `contracts`
// [ORB-22313]; they stay until a separately authorized API change.
pub use crate::application::contracts::{
    PrecedentAssistDemotion, PrecedentAssistEnablement, PrecedentAssistEnd, PrecedentAssistOutcome,
    PrecedentAssistOutcomeSource, PrecedentAssistState,
};
pub use enablement::{
    PRECEDENT_ASSIST_DEMOTION_WINDOW, PRECEDENT_ASSIST_HOLDBACK_SHARE, PRECEDENT_ASSIST_MAX_VETOES,
    PRECEDENT_ASSIST_WAIT_SECONDS, PRECEDENT_HALF_LIFE_DAYS, PrecedentAssistStatus,
};
pub use ledger::{PrecedentAssistRouting, PrecedentDecisionReceipt};
