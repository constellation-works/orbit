//! `orbit automation explain` [ORB-19990]: evaluate one definition's
//! condition over live workspace state and say what matches and why, beside
//! its durable state: arm states and holds, the stall, recent fires with
//! their upstream chain, and `self`. Nothing is written
//! (`docs/design/condition-triggers/1_scope.md` §9).
//!
//! `job:<name>` explains a job's auto-task steps instead ([`job`]).

mod job;
mod report;
mod runtime;
mod subjects;

pub use job::JobStepsExplain;
pub use report::*;

/// Most matches and closest misses one explain lists.
const MAX_LISTED: usize = 20;

/// The subject key of a workspace-scope condition.
const WORKSPACE_SUBJECT: &str = orbit_types::workflow::automation::CONDITION_WORKSPACE_SUBJECT;
