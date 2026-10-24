//! Constants and DTOs the application modules share.
//!
//! `task`, `automation`, `job`, `review`, `distributed`, `admission`,
//! `workspace_auto`, `pull_drain` and `sweep_filing` each name these, so
//! none of them owns them: a module that defined one would pull every other
//! user into a dependency on itself. This module sits below all nine and
//! imports none of them [ORB-17924]. Only pure values belong here: no
//! runtime access and no behaviour beyond formatting the value itself.

mod claimed_leaf;
mod crew_draw;
mod drain_entry;
mod host_provider_hold;
mod jobs;
mod pilot;
mod pull_settlement;
mod retry_key;
mod review_layer;

pub use claimed_leaf::{ClaimedLeafStage, DrainClaimedLeaf};
pub(crate) use crew_draw::{
    CreationCrewAssignment, CrewCandidate, InspectionPool, PROVIDER_LIMIT_REASSIGNMENT_PREFIX,
    PURPOSE_SOURCE_PREFIX, auto_task_minted, automatic_crew_source,
};
pub use drain_entry::DrainEntryPoint;
pub(crate) use host_provider_hold::{HostProviderHold, canonical_provider};
pub use jobs::{ESCALATION_TRIAGE_JOB, FINDING_VERIFICATION_JOB, PULL_DRAIN_JOB};
pub(crate) use pilot::PilotFingerprints;
pub(in crate::application) use pilot::SUPERSEDED_BY_SOURCE;
pub(in crate::application) use pull_settlement::SETTLEMENT_REFUSAL_REMEDY;
pub use pull_settlement::{PullSettlementEntry, PullSettlementOutcome};
pub(crate) use retry_key::RetryKey;
pub(crate) use review_layer::review_layer_json;
