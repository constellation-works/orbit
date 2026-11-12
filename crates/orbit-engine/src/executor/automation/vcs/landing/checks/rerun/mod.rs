//! Shared first-failure planner; never reduce mixed jobs to a retry permission.
//!
//! [`plan`] plans and admits the failed-jobs retry, [`send`] sends a reserved
//! retry and records its history, [`classification`] holds the unchanged-head
//! classification and no-edit repair reruns, and [`conclusion`] reads
//! conclusion-only and dependency-handoff failure sets.

mod classification;
mod conclusion;
mod plan;
mod send;

use orbit_types::workflow::CiRerunPlan;

pub(super) use classification::request_classification_rerun;
pub(in crate::executor::automation::vcs) use classification::request_no_edit_repair_rerun;
pub(in crate::executor::automation::vcs) use conclusion::handoff_step_set;
pub(in crate::executor::automation::vcs) use plan::plan_ci_rerun;
pub(super) use send::infrastructure_backoff;

pub(in crate::executor::automation::vcs) enum CiRerunStep {
    NotEligible,
    /// A durable infrastructure backoff; it never expires into source repair.
    Backoff(String),
    Wait(String),
    /// [ORB-21586] The retry's evidence could not be read or authenticated,
    /// for this reason. Unlike [`CiRerunStep::Wait`], nothing is in flight:
    /// the landing bounds how often it repeats before it escalates.
    Unreadable(String),
    Acted(Box<CiRerunPlan>),
    /// Admitted reruns are spent. The reason names the stopped jobs or the
    /// exhausted infrastructure classification and count; no repair runs.
    BudgetSpent(String),
}

/// The admission sentence for a head that already used its one CI retry.
const SHARED_RETRY_BUDGET_SPENT: &str = "the shared unchanged-head CI retry budget is spent";
