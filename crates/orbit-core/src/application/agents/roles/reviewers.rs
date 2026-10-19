//! Reviewers: the crew and model of the review gate attempts. Registered
//! with their terms; not computed yet.

use super::RoleSpec;
use crate::application::agents::score::Term;

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "settled reviews",
    terms: TERMS,
    rows: None,
};

const TERMS: &[Term] = &[
    Term {
        key: "findings_held",
        label: "Findings that held",
        weight: 1.0,
        definition: "Findings repaired by the reviewer and validated, or open findings on a reject that the next implementation run changed a cited path to address; each weighted by severity: critical 8, high 4, medium 2, low 1.",
        group: "credit",
    },
    Term {
        key: "fixes_held",
        label: "Fixes that held",
        weight: 1.0,
        definition: "accept_with_fixes verdicts whose fixes passed owner revalidation and whose task has no regression with escape review_missed.",
        group: "credit",
    },
    Term {
        key: "rejected_findings",
        label: "Rejected findings",
        weight: -1.0,
        definition: "Findings closed by an authorized decision (Disposed), and findings a review job filed as tasks that finding verification REJECTED.",
        group: "problem",
    },
    Term {
        key: "false_rejects",
        label: "False rejects",
        weight: -2.0,
        definition: "reject verdicts where the next implementation run changed none of the rejected findings' cited paths and the next review accepted.",
        group: "problem",
    },
    Term {
        key: "failed_fixes",
        label: "Failed fixes",
        weight: -1.0,
        definition: "Reviewer fixes that failed owner revalidation and were escalated.",
        group: "problem",
    },
    Term {
        key: "missed_regressions",
        label: "Missed regressions",
        weight: -4.0,
        definition: "Regressions with escape review_missed against tasks the row's review accepted, whenever filed.",
        group: "problem",
    },
    Term {
        key: "incomplete_reviews",
        label: "Incomplete reviews",
        weight: -0.5,
        definition: "incomplete verdicts not caused by infrastructure.",
        group: "problem",
    },
];
