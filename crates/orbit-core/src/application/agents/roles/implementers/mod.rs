//! Implementers: the crew and model of the implementation runs that did the
//! work. Escaped regressions and diff context are unscored. Every scored term is compared with the fleet on the same tier mix;
//! recorded cost and duration remain unscored context. Work admitted as
//! `no-diff-expected` (reviews, audits, sweeps) is not implementation and
//! counts nowhere here: no units, credit, charges or fleet expectation.
//! The tag is judged as of each attempt's start, so retagging a task after
//! work began cannot move that attempt out of scoring, and an untagged
//! task that ends with no diff still counts in full.
//!
//! Every count reads the tasks the selection's execution scope admits:
//! managed tasks by default, managed and interactive together, or
//! interactive ones alone. Cost and run time count managed tasks whatever
//! the scope, and the header's `execution_split` shows each row's managed,
//! interactive and unclassified tasks, so interactive work stays visible
//! while it is left out of the default counts.

mod context;
mod evidence;
mod performance;
mod size_curves;

pub(super) use evidence::{by_type_cells, per_task_rows};
pub(super) use size_curves::{TIERS as SIZE_CURVE_TIERS, size_curves};

pub(super) const ROLE: super::RoleSpec = super::RoleSpec {
    unit: "implemented tasks",
    terms: performance::TERMS,
    term_units: &[],
    rows: Some(performance::rows),
    header: Some(performance::execution_header),
};
