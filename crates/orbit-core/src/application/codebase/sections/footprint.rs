use super::*;

use super::rollup::*;

#[derive(Default)]
pub(in crate::application::codebase) struct FootprintRow<'a> {
    pub(in crate::application::codebase) tally: Tally,
    pub(in crate::application::codebase) widenings: usize,
    /// Each task that widened onto the path, with its latest such widening
    /// in the window: the numerator.
    pub(in crate::application::codebase) missed: Vec<(&'a TaskFacts, DateTime<Utc>)>,
}

/// Distinct tasks that had the path added mid-run by `context_files_widened`
/// ÷ tasks whose footprint touched it, for tasks active in the window.
/// Also returns the number of tasks active in the window.
pub(in crate::application::codebase) fn footprint_rows<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> (BTreeMap<String, FootprintRow<'a>>, usize) {
    let mut rows: BTreeMap<String, FootprintRow> = BTreeMap::new();
    let mut population = 0;
    for task in &facts.tasks {
        let widened: Vec<_> = task
            .widenings
            .iter()
            .filter(|(at, _)| params.within(*at))
            .collect();
        if widened.is_empty() && !params.within(task.updated_at) {
            continue;
        }
        population += 1;
        let mut missed: BTreeMap<String, (usize, DateTime<Utc>)> = BTreeMap::new();
        for (at, anchors) in &widened {
            for key in params.rollup.keys(anchors.iter()) {
                let entry = missed.entry(key).or_insert((0, *at));
                entry.0 += 1;
                entry.1 = entry.1.max(*at);
            }
        }
        let mut touched = params.rollup.keys(&task.footprint);
        touched.extend(missed.keys().cloned());
        for key in touched {
            let row = rows.entry(key.clone()).or_default();
            row.tally.denominator += 1;
            if let Some((events, at)) = missed.get(&key) {
                row.tally.numerator += 1;
                row.widenings += events;
                row.missed.push((task, *at));
            }
        }
    }
    (rows, population)
}

pub(in crate::application::codebase) fn footprint_misses(
    facts: &CodebaseFacts,
    params: &Params,
) -> Value {
    let (rows, population) = footprint_rows(facts, params);
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
        .map(|(path, row)| {
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({ "path": path, "widenings": row.widenings }),
            )
        })
        .collect();
    json!({ "rows": rows, "below_support": below_support, "population": population })
}
