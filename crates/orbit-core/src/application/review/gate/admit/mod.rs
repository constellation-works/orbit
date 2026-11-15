//! Admit a fresh reviewer for the committed, base-synchronized candidate.

mod admission;
mod holds;
mod output;
mod reviewer;

pub(crate) use output::review_gate_admit;
pub(super) use reviewer::reviewer_identity;
