//! Draw a task's crew from the complexity pools when it is created
//! [ORB-12717], and capture pool policy on the admitting pipeline so each
//! task's selection is frozen in its run input. Descendant pipelines inherit
//! the same task choice; a task created without a crew before assignment moved
//! to creation time, a stale pool assignment after rerating, or a default
//! fallback for an empty pool, is routed through the current pools at admission,
//! which reads the record and never writes it. The one exception is an
//! auto-task task whose explicit crew is provider-limited [ORB-17638]:
//! admission durably reassigns that task to its complexity pool and records
//! a warning comment on it. The other is `workflow.crew_selection =
//! "usage_pressure"` [ORB-17770]: creation, re-rate and admission draws weigh
//! each member by its crew's weekly usage pressure, and admission draws a
//! system-chosen crew again from its whole pool, persisting a changed crew
//! under the task's existing automatic source. Activity, review, verifier and
//! explorer pools use the same helper through `draw_crew_pool`, after their
//! existing membership filters, and carry each member's draw provenance.
//! `workflow.crew_performance` multiplies the creation, re-rate and
//! admission draws by each crew's performance factor from the persisted
//! snapshot through the same seam; it never re-draws an assigned crew by
//! itself, and activity draws take no performance factor.

mod activity;
mod admission;
mod candidates;
mod draw;
mod purpose;
mod types;

pub(crate) use admission::strip_auto_crew_admission;
pub(crate) use draw::{
    CrewPoolDraw, complexity_crews_key, complexity_of_crews_key, random_crew_ticket,
    seeded_crew_ticket,
};
pub(crate) use purpose::{
    PROVIDER_LIMIT_REASSIGNMENT_PREFIX, PURPOSE_SOURCE_PREFIX, auto_task_minted,
    automatic_crew_source,
};
pub(crate) use types::{CapturedCrewPools, CreationCrewAssignment, CrewCandidate, InspectionPool};
