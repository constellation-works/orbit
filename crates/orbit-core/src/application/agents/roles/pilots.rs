//! Pilots: the crew and model of the task-pilot run behind each task's
//! newest assessment. Registered with their terms; not computed yet.

use super::RoleSpec;
use crate::application::agents::score::Term;

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "assessed tasks",
    terms: TERMS,
    rows: None,
};

const TERMS: &[Term] = &[
    Term {
        key: "f1_lift",
        label: "F1 lift",
        weight: 1.0,
        definition: "F1 points gained over the filer's own selectors from before any pilot ran.",
        group: "credit",
    },
    Term {
        key: "tier_calibration",
        label: "Tier calibration",
        weight: 0.25,
        definition: "Percent of tasks whose assessed complexity matches their observed tier.",
        group: "credit",
    },
    Term {
        key: "costly_misses",
        label: "Costly misses",
        weight: -1.0,
        definition: "Landed files no pilot selector covered that caused a footprint block or a lock conflict.",
        group: "problem",
    },
    Term {
        key: "lock_wait_hours",
        label: "Lock-wait hours",
        weight: -1.0,
        definition: "Hours other tasks waited, in lock-wait episodes, on paths the holder reserved through a pilot selector that covered none of the holder's landed files.",
        group: "problem",
    },
];
