use super::*;

use super::rollup::*;

#[derive(Default)]
pub(in crate::application::codebase) struct RejectedRow<'a> {
    pub(in crate::application::codebase) tally: Tally,
    /// Each task rejected after a run in the window, with when: the
    /// numerator.
    pub(in crate::application::codebase) rejected: Vec<(&'a TaskFacts, DateTime<Utc>)>,
}

pub(in crate::application::codebase) struct RejectedRows<'a> {
    pub(in crate::application::codebase) rows: BTreeMap<String, RejectedRow<'a>>,
    counted: usize,
    excluded_without_run: usize,
}

/// Rejected tasks that had a job run ÷ tasks with a run whose footprint
/// touched the path. Proposal-stage rejections (no run) are excluded.
pub(in crate::application::codebase) fn rejected_rows<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> RejectedRows<'a> {
    let mut rows: BTreeMap<String, RejectedRow> = BTreeMap::new();
    let mut excluded_without_run = 0;
    let mut counted = 0;
    for task in &facts.tasks {
        let rejected_at = task
            .rejected_at
            .filter(|at| task.status == TaskStatus::Rejected && params.within(*at));
        if !task.had_run {
            excluded_without_run += usize::from(rejected_at.is_some());
            continue;
        }
        if rejected_at.is_none() && !params.within(task.updated_at) {
            continue;
        }
        counted += usize::from(rejected_at.is_some());
        for key in params.rollup.keys(&task.footprint) {
            let row = rows.entry(key).or_default();
            row.tally.denominator += 1;
            if let Some(at) = rejected_at {
                row.tally.numerator += 1;
                row.rejected.push((task, at));
            }
        }
    }
    RejectedRows {
        rows,
        counted,
        excluded_without_run,
    }
}

pub(in crate::application::codebase) fn rejected(facts: &CodebaseFacts, params: &Params) -> Value {
    let RejectedRows {
        rows,
        counted,
        excluded_without_run,
    } = rejected_rows(facts, params);
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
        .map(|(path, row)| {
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({ "path": path }),
            )
        })
        .collect();
    json!({
        "rows": rows,
        "below_support": below_support,
        "rejected_with_run": counted,
        "excluded_without_run": excluded_without_run,
    })
}
