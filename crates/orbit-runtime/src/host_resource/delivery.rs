//! What a delivery is to resource accounting: the job definitions its runs
//! use, and the class each run's input carries so its minutes, its profile
//! and the forecast all agree on one key and one clock.

use chrono::{DateTime, Utc};
pub use orbit_types::task::work_class;
use orbit_types::workflow;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Run-input key holding the delivery's [`DeliveryResourceClass`].
pub const RESOURCE_CLASS_KEY: &str = "resource_class";

/// The pipelines a submitted delivery is made of, whose submission captures
/// the class: a local delivery is a `task_auto_pipeline` →
/// `task_gate_pipeline` → `task_pr_pipeline` chain.
pub const SUBMITTED_DELIVERY_JOBS: &[&str] = &[
    "task_auto_pipeline",
    "task_gate_pipeline",
    workflow::TASK_PR_PIPELINE_JOB,
    workflow::TASK_LOCAL_PIPELINE_JOB,
];

/// Leaf definitions that only a claim may run; each claimed leaf is a whole
/// delivery. Owned by `orbit_types::workflow::CLAIMED_DELIVERY_LEAF_JOBS`;
/// this path stays for orbit-core's readers.
pub const CLAIMED_LEAF_JOBS: &[&str] = workflow::CLAIMED_DELIVERY_LEAF_JOBS;

/// Historical child jobs whose admission copied their parent's class.
/// Retained so stored explorer minutes still join the parent delivery's
/// profile rather than counting as a separate forecast delivery.
pub const ATTRIBUTED_CHILD_JOBS: &[&str] = &["agent_explore_pipeline"];

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
pub struct DeliveryResourceClass {
    pub task_id: String,
    pub work_class: String,
    #[serde(default)]
    pub complexity: Option<String>,
    /// When the delivery's first run was admitted.
    pub admitted_at: DateTime<Utc>,
    /// A resumed, replayed or retried delivery: its clock restarted part way
    /// through, so profiles exclude it.
    #[serde(default)]
    pub resumed: bool,
}

/// The class `input` carries for `task_id`, if any.
pub fn resource_class_in(input: &Value, task_id: &str) -> Option<DeliveryResourceClass> {
    serde_json::from_value::<DeliveryResourceClass>(input.get(RESOURCE_CLASS_KEY)?.clone())
        .ok()
        .filter(|class| class.task_id == task_id)
}
