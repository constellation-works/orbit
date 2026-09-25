//! The before-PR review gate: admit a fresh reviewer, then settle its
//! report into an honest verdict and certificate [ORB-11333].

mod admit;
mod context;
mod judgement;
mod settle;

pub(crate) use admit::review_gate_admit;
pub(crate) use settle::review_gate_settle;

#[cfg(test)]
mod tests;
