//! Settle the reviewer's report into an honest verdict and certificate.

mod candidate;
mod model;
mod orchestration;
mod output;
mod publication;

pub(super) use candidate::validation_scope;
pub(crate) use orchestration::review_gate_settle;
