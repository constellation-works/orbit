//! What task-pilot preparation and the state scheduler's member host agree
//! on: the reason a stale member settles with, the fingerprints of one
//! freshness read, and the atomic reservation decision.

use std::collections::{BTreeMap, BTreeSet};

/// The atomic pilot reservation decision, or an acquisition refusal before
/// the reservation operation ran. Operation errors remain errors.
pub(crate) enum PilotTaskReservation {
    Reserved(BTreeMap<String, BTreeSet<String>>),
    Deferred { reason: String },
}

/// The task outcome reason apply and prepare record for a task the branch
/// made stale under its claim; the member settles superseded, not failed.
pub(in crate::application) const SUPERSEDED_BY_SOURCE: &str = "superseded_by_source";

/// Both task-pilot hashes and the per-field digests behind a `material_changed`
/// refusal, from one instruction and dependency read. The hashes differ only
/// in the normalized status fields even when a dependency is updated
/// concurrently with preparation.
pub(crate) struct PilotFingerprints {
    pub(crate) material: String,
    pub(crate) status_neutral: String,
    pub(crate) components: BTreeMap<String, String>,
}
