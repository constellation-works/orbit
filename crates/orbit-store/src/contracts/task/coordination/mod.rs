//! Contracts for the durable task/reservation commit boundary.
//!
//! One task transition, its history, a file reservation, and the dependent
//! coordination rows an admission decision needs are published as a single
//! durable outcome. The boundary itself lives in
//! `repository::task::coordination`; these are the caller-visible parameter
//! and result shapes, free of any persistence technology.

mod admission;
mod branch_observation;
mod claim;
mod handoff;
mod journal;
mod settlement;

pub use admission::*;
pub use branch_observation::*;
pub use claim::*;
pub use handoff::*;
pub use journal::*;
pub use settlement::*;
