//! Audit reads and writes owned by the runtime: the run audit trail
//! projection, the run-events query its surfaces share, its CLI-invocation
//! log profiles, the system-authored audit seam, and the coordination-hold
//! audit events built on it, and the tag instructions and explore briefs a
//! run's activities received.

pub(super) mod coordination;
pub mod run;
pub mod system;
pub mod tag_instructions;

pub use run::events as run_events;
pub use run::logs as run_logs;
