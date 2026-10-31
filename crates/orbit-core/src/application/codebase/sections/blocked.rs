use super::*;

use super::rollup::*;

#[derive(Default)]
pub(in crate::application::codebase) struct BlockedRow<'a> {
    pub(in crate::application::codebase) tally: Tally,
    /// Each task a run failure blocked, or a base conflict blocked on this
    /// path, with its latest such block in the window: the numerator.
    pub(in crate::application::codebase) blocked: Vec<(&'a TaskFacts, DateTime<Utc>)>,
    /// Each task blocked in the window with no cause found and no run
    /// failure, with its latest such block.
    pub(in crate::application::codebase) unclassified: Vec<(&'a TaskFacts, DateTime<Utc>)>,
}

pub(in crate::application::codebase) struct BlockedRows<'a> {
    pub(in crate::application::codebase) rows: BTreeMap<String, BlockedRow<'a>>,
    causes: BTreeMap<&'static str, usize>,
    manual: BTreeMap<&'static str, usize>,
    attributed: usize,
    base_conflict: usize,
    not_failures: usize,
    unclassified: usize,
    /// The event of each unclassified task's latest unclassified block.
    unclassified_events: BTreeMap<String, usize>,
}

/// Tasks that entered `blocked` in the window. A block the run failure
/// classification attributes to infrastructure is reported under its cause,
/// never against the task's files; so is a manual block, by the kind of
/// actor that made it. A base conflict counts only against the conflicting
/// paths it recorded. A block that is no failure (an evidence hold, an
/// operator's cancel) is counted apart, and a block nothing classifies is
/// reported in the `unclassified` bucket and its own per-path column. Run
/// failures on the work count against the footprint: `blocked ÷ touched`.
pub(in crate::application::codebase) fn blocked_rows<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> BlockedRows<'a> {
    let mut rows: BTreeMap<String, BlockedRow> = BTreeMap::new();
    let mut causes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut manual: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut attributed = 0;
    let mut base_conflict = 0;
    let mut not_failures = 0;
    let mut unclassified = 0;
    let mut unclassified_events: BTreeMap<String, usize> = BTreeMap::new();
    for task in &facts.tasks {
        let blocks: Vec<&Block> = task
            .blocks
            .iter()
            .filter(|block| params.within(block.at))
            .collect();
        if blocks.is_empty() && !params.within(task.updated_at) {
            continue;
        }
        let mut task_causes = BTreeSet::new();
        let mut task_actors = BTreeSet::new();
        let mut run: Option<DateTime<Utc>> = None;
        let mut conflicted = false;
        let mut not_failure = false;
        let mut unknown: Option<&Block> = None;
        // Each conflicting path a base conflict recorded, with its latest.
        let mut conflicts: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        for block in &blocks {
            match &block.cause {
                BlockCause::Infra(name) => {
                    task_causes.insert(*name);
                }
                BlockCause::Run => run = run.max(Some(block.at)),
                BlockCause::BaseConflict(paths) => {
                    conflicted = true;
                    for key in params.rollup.keys(paths) {
                        let at = conflicts.entry(key).or_insert(block.at);
                        *at = (*at).max(block.at);
                    }
                }
                BlockCause::Manual(actor) => {
                    task_actors.insert(*actor);
                }
                BlockCause::NotFailure => not_failure = true,
                BlockCause::Unclassified => {
                    if unknown.is_none_or(|latest| block.at >= latest.at) {
                        unknown = Some(block);
                    }
                }
            }
        }
        for cause in task_causes {
            *causes.entry(cause).or_default() += 1;
        }
        for actor in task_actors {
            *manual.entry(actor).or_default() += 1;
        }
        let unknown = if run.is_some() { None } else { unknown };
        if let Some(block) = unknown {
            *unclassified_events.entry(block.event.clone()).or_default() += 1;
        }
        attributed += usize::from(run.is_some());
        base_conflict += usize::from(conflicted);
        not_failures += usize::from(not_failure);
        unclassified += usize::from(unknown.is_some());
        let mut touched = params.rollup.keys(&task.footprint);
        touched.extend(conflicts.keys().cloned());
        for key in touched {
            let row = rows.entry(key.clone()).or_default();
            row.tally.denominator += 1;
            if let Some(at) = run.max(conflicts.get(&key).copied()) {
                row.tally.numerator += 1;
                row.blocked.push((task, at));
            }
            if let Some(block) = unknown {
                row.unclassified.push((task, block.at));
            }
        }
    }
    BlockedRows {
        rows,
        causes,
        manual,
        attributed,
        base_conflict,
        not_failures,
        unclassified,
        unclassified_events,
    }
}

/// [`blocked_rows`] ranked. A path with only unclassified blocks still earns
/// a row, so that bucket stays visible per path; it ranks by its attributed
/// rate of zero.
pub(in crate::application::codebase) fn rank_blocked(
    rows: BTreeMap<String, BlockedRow<'_>>,
    min_support: usize,
) -> (Vec<(String, BlockedRow<'_>)>, usize) {
    rank_rows(
        rows,
        min_support,
        &|row: &BlockedRow| (row.tally.numerator, row.tally.denominator),
        |row| row.tally.numerator > 0 || !row.unclassified.is_empty(),
    )
}

/// Unclassified events the Blocked section names, most frequent first.
const UNCLASSIFIED_EVENT_LIMIT: usize = 5;

pub(in crate::application::codebase) fn blocked(facts: &CodebaseFacts, params: &Params) -> Value {
    let BlockedRows {
        rows,
        causes,
        manual,
        attributed,
        base_conflict,
        not_failures,
        unclassified,
        unclassified_events,
    } = blocked_rows(facts, params);
    let (ranked, below_support) = rank_blocked(rows, params.min_support);
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
        .map(|(path, row)| {
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({ "path": path, "unclassified": row.unclassified.len() }),
            )
        })
        .collect();
    let mut cause_rows: Vec<Value> = INFRA_CAUSES
        .iter()
        .filter_map(|cause| {
            let tasks = causes.get(cause).copied().unwrap_or(0);
            (tasks > 0).then(|| json!({ "cause": cause, "kind": "infrastructure", "tasks": tasks }))
        })
        .collect();
    cause_rows.extend(ACTOR_KINDS.iter().filter_map(|actor| {
        let tasks = manual.get(actor).copied().unwrap_or(0);
        (tasks > 0)
            .then(|| json!({ "cause": "manual", "kind": "manual", "actor": actor, "tasks": tasks }))
    }));
    if base_conflict > 0 {
        cause_rows.push(
            json!({ "cause": "base_conflict", "kind": "base_conflict", "tasks": base_conflict }),
        );
    }
    cause_rows
        .push(json!({ "cause": "unclassified", "kind": "unclassified", "tasks": unclassified }));
    cause_rows.push(json!({ "cause": "run_failure", "kind": "attributed", "tasks": attributed }));
    let mut events: Vec<(String, usize)> = unclassified_events.into_iter().collect();
    events.sort_by(|(left_event, left), (right_event, right)| {
        right.cmp(left).then_with(|| left_event.cmp(right_event))
    });
    let events: Vec<Value> = events
        .into_iter()
        .take(UNCLASSIFIED_EVENT_LIMIT)
        .map(|(event, tasks)| json!({ "event": event, "tasks": tasks }))
        .collect();
    json!({
        "rows": rows,
        "below_support": below_support,
        "causes": cause_rows,
        "unclassified_events": events,
        "not_failures": not_failures,
    })
}
