//! Audit reads and writes owned by the runtime: the run audit trail
//! projection, the system-authored audit seam, and the coordination-hold
//! audit events built on it.

pub(super) mod coordination;
pub mod run;
mod run_projection;
pub(crate) mod system;
