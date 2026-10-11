//! Audit reads and writes owned by the runtime: the run audit trail
//! projection and the coordination-hold audit seam.

pub(super) mod coordination;
pub mod run;
pub(crate) mod run_detail;
mod run_projection;
