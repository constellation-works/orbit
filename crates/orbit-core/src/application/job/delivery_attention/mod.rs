//! Delivery attention for `review` (and blocked-with-PR) tasks, derived
//! only from durable run, history and ledger state. No forge reads.

mod history;
mod query;
mod review_gate;
mod value;

pub use review_gate::LANDING_REVIEW_SETTLE_STEP;
pub use review_gate::landing_review_gate_facts;
pub use value::delivery_attention_value;
