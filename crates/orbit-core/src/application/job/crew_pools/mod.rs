//! Draw a task's crew from the complexity pools when it is created
//! [ORB-12717], and capture pool policy on the admitting pipeline so each
//! task's selection is frozen in its run input. Descendant pipelines inherit
//! the same task choice; a task created without a crew before assignment moved
//! to creation time, a stale pool assignment after rerating, or a default
//! fallback for an empty pool, is routed through the current pools at admission,
//! which reads the record and never writes it.

mod activity;
mod admission;
mod candidates;
mod draw;
mod purpose;
mod types;

pub(crate) use admission::strip_auto_crew_admission;
pub(crate) use draw::{
    complexity_crews_key, complexity_of_crews_key, random_crew_ticket, seeded_crew_ticket,
    weighted_draw,
};
pub(crate) use purpose::{PURPOSE_SOURCE_PREFIX, automatic_crew_source};
pub(crate) use types::{CapturedCrewPools, CreationCrewAssignment, CrewCandidate, InspectionPool};
