//! Constants and DTOs the application modules share.
//!
//! `task`, `automation`, `job`, `review`, `distributed`, `admission`,
//! `workspace_auto`, `pull_drain` and `sweep_filing` each name these, so
//! none of them owns them: a module that defined one would pull every other
//! user into a dependency on itself. This module sits below all nine and
//! imports none of them [ORB-17924]. Only pure values belong here: no
//! runtime access and no behaviour beyond formatting the value itself.

mod drain_entry;
mod jobs;
mod pilot;
mod pull_settlement;
mod review_layer;

pub use drain_entry::DrainEntryPoint;
pub use jobs::{ESCALATION_TRIAGE_JOB, FINDING_VERIFICATION_JOB, PULL_DRAIN_JOB};
pub(crate) use pilot::PilotFingerprints;
pub(in crate::application) use pilot::SUPERSEDED_BY_SOURCE;
pub(in crate::application) use pull_settlement::SETTLEMENT_REFUSAL_REMEDY;
pub use pull_settlement::{PullSettlementEntry, PullSettlementOutcome};
pub(crate) use review_layer::review_layer_json;
