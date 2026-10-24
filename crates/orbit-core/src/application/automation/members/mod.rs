//! Narrow Core adapter for the shared state scheduling domain.

mod claim;
mod host;
mod member_host;
mod stale;

pub(crate) use claim::{bundle_crews, claim, require_claim_owner};
pub(crate) use host::{evaluate, evaluate_with_cache};
pub(crate) use stale::stale_tasks;

use super::{pins, preparation::InstructionSnapshot};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::{members::*, *};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub(crate) struct Host<'a> {
    runtime: &'a OrbitRuntime,
    /// The state routine this consumer serves; admitted runs name it as their
    /// trigger [ORB-13016].
    routine: &'a str,
    trigger: &'a StateTrigger,
    /// Status inspection and sweep previews resolve cached refs without
    /// fetching; only a real evaluation refreshes origin.
    dry_run: bool,
    /// The one policy this consumer observes, admits and fingerprints with
    /// [ORB-12745, ORB-13638].
    policy: PreparationPolicy,
    incidents: RefCell<super::incidents::IncidentSession>,
    instructions: RefCell<BTreeMap<String, InstructionSnapshot>>,
    /// Attempt id → the commit its source pin names, resolved once per
    /// attempt to carry pre-upgrade assessments forward.
    pinned: RefCell<BTreeMap<String, Option<String>>>,
    /// The pin namespace owner, resolved once [ORB-14164].
    owner: RefCell<Option<pins::Owner>>,
    /// Resolve once per evaluation, best-effort fetching only outside dry
    /// runs, shared by page observation, reconciliation and admission.
    head: RefCell<Option<(String, SourceRevision)>>,
    inspection_cache: Option<&'a super::source_cache::SourceCache>,
    /// One provider-hold scan per evaluation clock, shared by observation and
    /// admission. Observation replaces it when its clock differs.
    pilot_hold: RefCell<Option<PilotHoldCache>>,
}

struct PilotHoldCache {
    now: DateTime<Utc>,
    withhold: Option<String>,
}
