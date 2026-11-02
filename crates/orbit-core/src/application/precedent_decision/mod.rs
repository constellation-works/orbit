//! Shadow precedent-decision consumer. Default off. A sealed prediction is
//! private ledger state and is never copied onto a decision surface.
mod contract;
mod export;
mod ledger;
mod runtime;

pub use contract::{PRECEDENT_DECISION_USE_CASE, precedent_decision_question_template};
pub use ledger::PrecedentDecisionReceipt;
