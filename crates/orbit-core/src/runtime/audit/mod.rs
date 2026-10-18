//! Audit reads and writes owned by the runtime: the run audit trail
//! projection, the system-authored audit seam, and the coordination-hold
//! audit events built on it, and the tag instructions a run's activities
//! received.

pub(super) mod coordination;
pub mod run;
pub(crate) mod run_detail;
mod run_projection;
pub(crate) mod system;
pub mod tag_instructions;
