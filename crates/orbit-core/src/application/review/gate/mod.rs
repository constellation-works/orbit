//! The before-PR review gate: admit a fresh reviewer, then settle its
//! report into an honest verdict and certificate [ORB-11333].

mod admit;
mod baseline;
mod context;
mod host_evidence;
mod judgement;
mod release;
mod settle;

pub(crate) use admit::review_gate_admit;
pub(crate) use release::{record_reviewer_invocation, release_review_attempt};
pub(crate) use settle::review_gate_settle;

#[cfg(test)]
mod tests;
