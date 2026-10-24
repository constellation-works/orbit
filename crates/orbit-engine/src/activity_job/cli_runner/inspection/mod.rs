//! Read-only source inspection for a CLI provider run.
//!
//! `checkout` leases and materializes the pinned source checkout.
//! `tools` admits the run and supplies its native tool arguments.

mod checkout;
mod tools;

pub(super) use checkout::SourceInspection;
pub use checkout::is_source_inspection_checkout;
pub(super) use tools::{admit_inspection, prepare_inspection_tools};
