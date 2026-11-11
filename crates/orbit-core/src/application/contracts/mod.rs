//! Constants and DTOs the application modules share.
//!
//! `task`, `automation`, `job`, `review`, `distributed`, `admission`,
//! `workspace_auto`, `pull_drain` and `sweep_filing` each name these, so
//! none of them owns them: a module that defined one would pull every other
//! user into a dependency on itself. This module sits below all nine and
//! imports none of them [ORB-17924], nor `routines` or `auto_tasks` [ORB-22623]. Only pure values
//! belong here: no runtime access and no behaviour beyond formatting the value
//! itself. There are two exceptions: a port trait that a layer above
//! application implements, such as `ToolInvocationPort` for the adapter
//! [ORB-22004] and `TaskBundleMaintenancePort` for bootstrap [ORB-22005]; and
//! the clock settings, which read and write the host's `clock.toml` under a root
//! their caller names, because `automation` needs the cadence below `routines`.
//! They are owned by the `orbit-clock` crate and re-exported here [ORB-22688].

mod claimed_leaf;
mod crew_draw;
mod distributed_gate;
mod drain_entry;
mod experiment;
mod filing_tags;
mod host_provider_hold;
mod jobs;
mod owner_source_identity;
mod pilot;
mod published_delivery;
mod pull_settlement;
mod retry_key;
mod review_layer;
mod schedule_display;
mod task_bundle_maintenance;
mod tool_invocation;

pub use claimed_leaf::{ClaimedLeafStage, DrainClaimedLeaf};
pub(crate) use crew_draw::{
    AutoEffortSubject, CreationCrewAssignment, CrewCandidate, CrewPoolDraw,
    INSPECTION_CREW_UNAVAILABLE, InspectionPool, PROVIDER_LIMIT_REASSIGNMENT_PREFIX,
    auto_effort_note, auto_task_minted,
};
pub use distributed_gate::DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED;
pub use drain_entry::DrainEntryPoint;
pub(in crate::application) use experiment::ExperimentCrewPool;
pub(crate) use filing_tags::{
    CI_FAILURE_TAG, CODE_SCANNING_TAG, DEPENDABOT_TAG, RECONCILIATION_FOLLOW_UP_TAG_PREFIX,
};
pub(crate) use host_provider_hold::{HostProviderHold, canonical_provider};
pub use jobs::{ESCALATION_TRIAGE_JOB, FINDING_VERIFICATION_JOB, PULL_DRAIN_JOB};
pub use orbit_clock::{
    ClockSettings, DEFAULT_CLOCK_CADENCE_SECONDS, LAUNCHD_LABEL, SYSTEMD_UNIT, load_clock_settings,
    save_clock_settings,
};
pub(crate) use owner_source_identity::OwnerSourceIdentityRefusal;
pub(in crate::application) use pilot::SUPERSEDED_BY_SOURCE;
pub(crate) use pilot::{PilotFingerprints, PilotTaskReservation};
pub(crate) use published_delivery::PublishedDelivery;
pub(in crate::application) use pull_settlement::SETTLEMENT_REFUSAL_REMEDY;
pub use pull_settlement::{PullSettlementEntry, PullSettlementOutcome};
pub(crate) use retry_key::RetryKey;
pub(crate) use review_layer::review_layer_json;
pub use schedule_display::ScheduleDisplayState;
pub(crate) use task_bundle_maintenance::TaskBundleMaintenancePort;
pub(crate) use tool_invocation::ToolInvocationPort;
