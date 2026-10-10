//! What a host resource throttle still lets start [ORB-14624].
//!
//! CPU pressure from cargo-heavy leaves used to hold every admission, so a
//! frozen review batch could wait out its retry budget behind them. A review,
//! curation or full-review auto-task reads code and writes artifacts; it adds
//! almost no CPU. While CPU alone holds admissions, such leaves start up to a
//! small reserved budget. Memory and disk pressure still hold them: an agent
//! session costs memory, and its artifacts cost disk.
//!
//! The drain's classifier and the readiness diagnostic both take their answer
//! from [`resource_gate`], so they cannot disagree about who starts.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use orbit_types::workflow::{AUTO_TASK_TAG_PREFIX, ResourceThrottle};
use serde_json::{Value, json};

/// Readiness reason for a CPU-light leaf held only because every reserved
/// light slot is taken.
pub(in crate::adapter::engine_host::v2_host) const CPU_LIGHT_BUDGET_FULL: &str =
    "cpu_light_budget_full";

/// A leaf that adds almost no CPU: an auto-task tagged `no-diff-expected`.
/// Both tags are required. `no-diff-expected` alone is an operator's claim
/// about the diff, not about the work; automated provenance alone says
/// nothing about cost.
pub(in crate::adapter::engine_host::v2_host) fn is_cpu_light(task: &Task) -> bool {
    task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
        && task
            .tags
            .iter()
            .any(|tag| tag.starts_with(AUTO_TASK_TAG_PREFIX))
}

/// Reserved light slots, and how many live leaves already hold one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::adapter::engine_host::v2_host) struct LightBudget {
    pub(in crate::adapter::engine_host::v2_host) reserved: usize,
    pub(in crate::adapter::engine_host::v2_host) active: usize,
}

impl LightBudget {
    /// Count the live light leaves among `claimed` against `reserved`.
    ///
    /// Live means a pending or running leaf wrapper names the task; a light
    /// task stuck `in-progress` with no run does not hold a slot forever.
    pub(in crate::adapter::engine_host::v2_host) fn new<'a>(
        reserved: u8,
        claimed: impl IntoIterator<Item = &'a String>,
        task_lookup: &BTreeMap<String, Task>,
    ) -> Self {
        let active = claimed
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|task_id| task_lookup.get(*task_id).is_some_and(is_cpu_light))
            .count();
        Self {
            reserved: usize::from(reserved),
            active,
        }
    }

    pub(in crate::adapter::engine_host::v2_host) fn remaining(self) -> usize {
        self.reserved.saturating_sub(self.active)
    }

    /// The budget as readiness and the drain report it. `applies` says
    /// whether a CPU-only throttle is spending it right now.
    pub(in crate::adapter::engine_host::v2_host) fn to_json(self, applies: bool) -> Value {
        json!({
            "reserved": self.reserved,
            "active": self.active,
            "remaining": self.remaining(),
            "applies": applies,
        })
    }

    /// Why a light leaf waits when the budget is spent.
    pub(in crate::adapter::engine_host::v2_host) fn full_detail(self) -> String {
        format!(
            "{} of {} reserved CPU-light leaves are running under the CPU throttle \
             (workflow.resource_throttle.cpu_light_leaves); this task starts when one finishes \
             or CPU falls below its resume mark",
            self.active, self.reserved
        )
    }
}

/// What host pressure leaves admissible this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::adapter::engine_host::v2_host) enum ResourceGate {
    /// No throttle holds: every free slot is open.
    Open,
    /// Only CPU holds, and light slots are reserved: CPU-light leaves fill
    /// up to the budget's remaining slots; everything else waits.
    LightOnly,
    /// Memory or disk holds, or no light slot is reserved: nothing starts.
    Closed,
}

impl ResourceGate {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        throttle: Option<&ResourceThrottle>,
        budget: &LightBudget,
    ) -> Self {
        let Some(throttle) = throttle else {
            return Self::Open;
        };
        let cpu_only = !throttle.resources.is_empty()
            && throttle
                .resources
                .iter()
                .all(|pressure| pressure.resource == "cpu");
        if cpu_only && budget.reserved > 0 {
            Self::LightOnly
        } else {
            Self::Closed
        }
    }

    /// The slots a wave may fill, given the slots free of live leaves.
    pub(in crate::adapter::engine_host::v2_host) fn free_slots(
        self,
        unthrottled: usize,
        budget: &LightBudget,
    ) -> usize {
        match self {
            Self::Open => unthrottled,
            Self::LightOnly => unthrottled.min(budget.remaining()),
            Self::Closed => 0,
        }
    }

    /// Whether `task` may take one of this pass's slots at all.
    pub(in crate::adapter::engine_host::v2_host) fn admits(self, task: &Task) -> bool {
        match self {
            Self::Open => true,
            Self::LightOnly => is_cpu_light(task),
            Self::Closed => false,
        }
    }
}
