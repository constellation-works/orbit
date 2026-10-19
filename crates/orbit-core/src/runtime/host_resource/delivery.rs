//! What a delivery is to resource accounting: the job definitions its runs
//! use, and the class each run's input carries so its minutes, its profile
//! and the forecast all agree on one key and one clock.

use chrono::{DateTime, Utc};
pub(crate) use orbit_types::task::work_class;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Run-input key holding the delivery's [`DeliveryResourceClass`].
pub(crate) const RESOURCE_CLASS_KEY: &str = "resource_class";

/// The pipelines a submitted delivery is made of, whose submission captures
/// the class: a local delivery is a `task_auto_pipeline` →
/// `task_gate_pipeline` → `task_pr_pipeline` chain.
pub(crate) const SUBMITTED_DELIVERY_JOBS: &[&str] = &[
    "task_auto_pipeline",
    "task_gate_pipeline",
    "task_pr_pipeline",
    "task_local_pipeline",
];

/// Leaf definitions that only a claim may run; each claimed leaf is a whole
/// delivery.
pub(crate) const CLAIMED_LEAF_JOBS: &[&str] =
    &["task_claimed_local_pipeline", "task_claimed_pr_pipeline"];

/// Definitions whose runs a delivery delegates without taking a slot of
/// their own: a read-only explorer an implement or review-repair agent
/// invoked. Its admission copies the delegating run's class into its input,
/// so its minutes join that delivery's profile; it is never a delivery the
/// forecast counts.
pub(crate) const ATTRIBUTED_CHILD_JOBS: &[&str] =
    &[orbit_types::workflow::activity_job::EXPLORE_JOB];

/// Every job definition whose runs belong to a delivery.
pub(crate) fn delivery_jobs() -> Vec<&'static str> {
    SUBMITTED_DELIVERY_JOBS
        .iter()
        .chain(CLAIMED_LEAF_JOBS)
        .copied()
        .collect()
}

/// One delivery's profile key and clock, fixed when it was admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeliveryResourceClass {
    pub(crate) task_id: String,
    pub(crate) work_class: String,
    #[serde(default)]
    pub(crate) complexity: Option<String>,
    /// When the delivery's first run was admitted.
    pub(crate) admitted_at: DateTime<Utc>,
    /// A resumed, replayed or retried delivery: its clock restarted part way
    /// through, so profiles exclude it.
    #[serde(default)]
    pub(crate) resumed: bool,
}

/// The class `input` carries for `task_id`, if any.
pub(crate) fn resource_class_in(input: &Value, task_id: &str) -> Option<DeliveryResourceClass> {
    serde_json::from_value::<DeliveryResourceClass>(input.get(RESOURCE_CLASS_KEY)?.clone())
        .ok()
        .filter(|class| class.task_id == task_id)
}
