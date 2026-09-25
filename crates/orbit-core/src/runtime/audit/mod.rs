//! Audit reads and writes owned by the runtime: the run audit trail
//! projection and the coordination-hold audit seam.

pub(super) mod coordination;
pub mod run;
mod run_projection;

#[cfg(test)]
mod tests;
