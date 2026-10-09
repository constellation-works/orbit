//! Narrow Core adapter for the shared state scheduling domain.

mod claim;
mod host;
mod member_host;
mod stale;

pub(crate) use claim::claim;
pub(crate) use host::evaluate;
pub(crate) use stale::stale_tasks;

use super::{pins, preparation::InstructionSnapshot};
use crate::OrbitRuntime;
use orbit_types::workflow::automation::{members::*, *};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub(crate) struct Host<'a> {
    runtime: &'a OrbitRuntime,
    /// The state routine this consumer serves; admitted runs name it as their
    /// trigger [ORB-13016].
    routine: &'a str,
    trigger: &'a StateTrigger,
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
    /// Resolve (and best-effort fetch) once per evaluation, shared by page
    /// observation, reconciliation and admission.
    head: RefCell<Option<(String, SourceRevision)>>,
}

/// The task outcome reason apply and prepare record for a task the branch
/// made stale under its claim; the member settles superseded, not failed.
pub(crate) const SUPERSEDED_BY_SOURCE: &str = "superseded_by_source";
