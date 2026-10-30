//! The crew ladder: the operator's `workflow.crew_ladder.rungs`, cheapest
//! first, and one standing trial per listed complexity tier for the rung
//! directly below the tier's floor (its cheapest rung in the configured
//! pool).
//!
//! Phase 1 runs each trial as a system experiment in experiment `shadow`
//! mode, owned by `system:crew-ladder`, so assignments are recorded and no
//! task's crew changes. `plan` decides, per tier, the floor, the candidate
//! and whether a trial may run; `trials` reconciles the stored experiments
//! with that plan at delivery admission; `report` shows both, read-only.

mod plan;
mod report;
mod trials;

pub use report::{CrewLadderReason, CrewLadderReport, CrewLadderRow, CrewLadderTrialView};

/// The actor that creates, starts and concludes ladder trials, and the
/// `created_by` that marks an experiment as one.
pub const CREW_LADDER_ACTOR: &str = "system:crew-ladder";
