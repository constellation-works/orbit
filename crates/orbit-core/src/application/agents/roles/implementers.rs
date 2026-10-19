//! Implementers: the crew and model of the implementation runs that did the
//! work. Registered with their terms; not computed yet.

use super::RoleSpec;
use crate::application::agents::score::Term;

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "implemented tasks",
    terms: TERMS,
    rows: None,
};

const TERMS: &[Term] = &[
    Term {
        key: "weighted_landed",
        label: "Weighted landed",
        weight: 1.0,
        definition: "Landed tasks whose final implementation run is the row's, weighted by complexity: low 1, medium 2, hard 4, xhard 8, unassessed or unset 1.",
        group: "credit",
    },
    Term {
        key: "first_pass_accepts",
        label: "First-pass accepts",
        weight: 1.0,
        definition: "Reviewed tasks whose first review verdict on the row's candidate was accept.",
        group: "credit",
    },
    Term {
        key: "review_rejects",
        label: "Review rejects",
        weight: -1.0,
        definition: "reject verdicts on the row's candidates.",
        group: "problem",
    },
    Term {
        key: "reviewer_repairs",
        label: "Reviewer repairs",
        weight: -0.5,
        definition: "accept_with_fixes verdicts on the row's candidates.",
        group: "problem",
    },
    Term {
        key: "crew_rework",
        label: "Crew-caused rework",
        weight: -1.0,
        definition: "Repeat runs, [BLOCKED] or unmerged PRs, and operator requeues with cause crew, charged to the run that ended the attempt. Review verdicts are not counted again here.",
        group: "problem",
    },
    Term {
        key: "escaped_regressions",
        label: "Escaped regressions",
        weight: -2.0,
        definition: "Regressions with escape no_test against tasks whose final implementation run is the row's.",
        group: "problem",
    },
];
