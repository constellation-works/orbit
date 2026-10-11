//! The six rankings of the codebase view, computed from cached scan facts for
//! one window, minimum support and rollup.
//!
//! Every ranking is a rate over distinct tasks (or reservations, for
//! coupling) with its numerator and denominator, so a popular file does not
//! lead merely because many tasks touch it. Rows whose denominator is below
//! the minimum support are omitted and counted in `below_support`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_core::TaskStatus;
use serde_json::{Value, json};

use super::facts::{Anchor, BlockCause, CodebaseFacts, INFRA_CAUSES, TaskFacts};

/// Rows returned per ranking.
const ROW_LIMIT: usize = 25;
/// Regressed tasks listed in one regression row's drill-down.
const DRILLDOWN_LIMIT: usize = 20;
/// Reservations naming more files than this are left out of change coupling,
/// so a sweeping reservation does not pair everything with everything.
pub(super) const LARGE_FOOTPRINT_CAP: usize = 12;

/// How footprint paths are grouped before counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rollup {
    File,
    Dir,
    Crate,
}

impl Rollup {
    pub(super) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "file" => Some(Self::File),
            "dir" => Some(Self::Dir),
            "crate" => Some(Self::Crate),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Crate => "crate",
        }
    }

    /// The group an anchor counts under. Directories end in `/`; files at the
    /// repository root roll up to `./`.
    fn key(self, anchor: &Anchor) -> String {
        let as_dir = |path: &str| format!("{path}/");
        match self {
            Self::File if anchor.dir => as_dir(&anchor.path),
            Self::File => anchor.path.clone(),
            Self::Dir if anchor.dir => as_dir(&anchor.path),
            Self::Dir => anchor
                .path
                .rsplit_once('/')
                .map_or_else(|| "./".to_string(), |(parent, _)| as_dir(parent)),
            Self::Crate => {
                let mut parts = anchor.path.split('/');
                let first = parts.next().unwrap_or_default();
                let second = parts.next();
                match second {
                    Some(name) if first == "crates" => as_dir(&format!("crates/{name}")),
                    Some(_) => as_dir(first),
                    None if anchor.dir => as_dir(first),
                    None => "./".to_string(),
                }
            }
        }
    }

    fn keys<'a>(self, anchors: impl IntoIterator<Item = &'a Anchor>) -> BTreeSet<String> {
        anchors.into_iter().map(|anchor| self.key(anchor)).collect()
    }
}

pub(super) struct Params {
    pub(super) since: Option<DateTime<Utc>>,
    pub(super) min_support: usize,
    pub(super) rollup: Rollup,
}

impl Params {
    fn within(&self, at: DateTime<Utc>) -> bool {
        self.since.is_none_or(|since| at >= since)
    }
}

/// A rate row before rendering: `numerator` of `denominator` distinct items.
#[derive(Default)]
struct Tally {
    numerator: usize,
    denominator: usize,
}

fn rate(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Keep rows with a numerator and enough support, ranked by rate, then
/// numerator, then path; report how many fell below the support threshold.
fn rank<T>(
    rows: BTreeMap<String, T>,
    min_support: usize,
    tally: impl Fn(&T) -> (usize, usize),
) -> (Vec<(String, T)>, usize) {
    rank_rows(rows, min_support, &tally, |row| tally(row).0 > 0)
}

/// [`rank`] with an explicit test for whether a row has anything to show.
fn rank_rows<T>(
    rows: BTreeMap<String, T>,
    min_support: usize,
    tally: &impl Fn(&T) -> (usize, usize),
    shown: impl Fn(&T) -> bool,
) -> (Vec<(String, T)>, usize) {
    let mut below_support = 0;
    let mut kept: Vec<(String, T)> = rows
        .into_iter()
        .filter(|(_, row)| {
            let (_, denominator) = tally(row);
            if !shown(row) {
                return false;
            }
            if denominator < min_support {
                below_support += 1;
                return false;
            }
            true
        })
        .collect();
    kept.sort_by(|(left_path, left), (right_path, right)| {
        let (left_num, left_den) = tally(left);
        let (right_num, right_den) = tally(right);
        rate(right_num, right_den)
            .total_cmp(&rate(left_num, left_den))
            .then_with(|| right_num.cmp(&left_num))
            .then_with(|| left_path.cmp(right_path))
    });
    kept.truncate(ROW_LIMIT);
    (kept, below_support)
}

fn rate_fields(numerator: usize, denominator: usize) -> Value {
    json!({
        "rate": rate(numerator, denominator),
        "numerator": numerator,
        "denominator": denominator,
    })
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

pub(super) fn all_sections(facts: &CodebaseFacts, params: &Params) -> Value {
    json!({
        "regressions": regressions(facts, params),
        "lock_contention": lock_contention(facts, params),
        "footprint_misses": footprint_misses(facts, params),
        "change_coupling": change_coupling(facts, params),
        "blocked": blocked(facts, params),
        "rejected": rejected(facts, params),
    })
}

/// Distinct done tasks with an incoming `regression_from` ÷ done tasks whose
/// footprint touched the path, for tasks that became done in the window.
fn regressions(facts: &CodebaseFacts, params: &Params) -> Value {
    #[derive(Default)]
    struct Row<'a> {
        tally: Tally,
        regressed: Vec<&'a TaskFacts>,
        by_crew: BTreeMap<String, Tally>,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut population = 0;
    for task in &facts.tasks {
        let Some(done_at) = task.done_at else {
            continue;
        };
        if !params.within(done_at) {
            continue;
        }
        population += 1;
        let regressed = !task.regressed_by.is_empty();
        let crew = task
            .crew
            .clone()
            .unwrap_or_else(|| "unassigned".to_string());
        for key in params.rollup.keys(&task.footprint) {
            let row = rows.entry(key).or_default();
            row.tally.denominator += 1;
            let crew_tally = row.by_crew.entry(crew.clone()).or_default();
            crew_tally.denominator += 1;
            if regressed {
                row.tally.numerator += 1;
                crew_tally.numerator += 1;
                row.regressed.push(task);
            }
        }
    }
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(path, mut row)| {
            row.regressed
                .sort_by_key(|regressed| std::cmp::Reverse(regressed.done_at));
            let regressed: Vec<Value> = row
                .regressed
                .iter()
                .take(DRILLDOWN_LIMIT)
                .map(|task| {
                    json!({
                        "task_id": task.id,
                        "title": task.title,
                        "crew": task.crew,
                        "regression_tasks": task.regressed_by,
                    })
                })
                .collect();
            let mut by_crew: Vec<(String, Tally)> = row.by_crew.into_iter().collect();
            by_crew.sort_by(|(left_crew, left), (right_crew, right)| {
                right
                    .numerator
                    .cmp(&left.numerator)
                    .then_with(|| right.denominator.cmp(&left.denominator))
                    .then_with(|| left_crew.cmp(right_crew))
            });
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({
                    "path": path,
                    "regressed_tasks": regressed,
                    "by_crew": by_crew
                        .into_iter()
                        .map(|(crew, tally)| merge(
                            rate_fields(tally.numerator, tally.denominator),
                            json!({ "crew": crew }),
                        ))
                        .collect::<Vec<_>>(),
                }),
            )
        })
        .collect();
    json!({ "rows": rows, "below_support": below_support, "population": population })
}

/// Distinct tasks that waited on a path because of a lock conflict ÷
/// distinct tasks that asked for it (reserved it or were refused it). A retry
/// loop repeats the same contender and counts once. Reservation and denial
/// counts are reported beside, not folded into, the contention figure.
fn lock_contention(facts: &CodebaseFacts, params: &Params) -> Value {
    #[derive(Default)]
    struct Row {
        contenders: BTreeSet<String>,
        requesters: BTreeSet<String>,
        reservations: usize,
        denials: usize,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for reservation in &facts.reservations {
        if !params.within(reservation.at) {
            continue;
        }
        for key in params.rollup.keys(&reservation.files) {
            let row = rows.entry(key).or_default();
            row.reservations += 1;
            row.requesters.extend(reservation.task_ids.iter().cloned());
        }
    }
    for denial in &facts.lock_denials {
        if !params.within(denial.at) {
            continue;
        }
        for key in params.rollup.keys(&denial.files) {
            let row = rows.entry(key).or_default();
            row.denials += 1;
            row.contenders.extend(denial.contenders.iter().cloned());
            row.requesters.extend(denial.contenders.iter().cloned());
        }
    }
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.contenders.len(), row.requesters.len())
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(path, row)| {
            merge(
                rate_fields(row.contenders.len(), row.requesters.len()),
                json!({
                    "path": path,
                    "contending_tasks": row.contenders.len(),
                    "reservations": row.reservations,
                    "denials": row.denials,
                    // Lock waits are not persisted with a duration.
                    "wait_ms": Value::Null,
                }),
            )
        })
        .collect();
    json!({
        "rows": rows,
        "below_support": below_support,
        "wait_time_recorded": false,
    })
}

/// Distinct tasks that had the path added mid-run by `context_files_widened`
/// ÷ tasks whose footprint touched it, for tasks active in the window.
fn footprint_misses(facts: &CodebaseFacts, params: &Params) -> Value {
    #[derive(Default)]
    struct Row {
        tally: Tally,
        widenings: usize,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
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
        let mut missed: BTreeMap<String, usize> = BTreeMap::new();
        for (_, anchors) in &widened {
            for key in params.rollup.keys(anchors.iter()) {
                *missed.entry(key).or_default() += 1;
            }
        }
        let mut touched = params.rollup.keys(&task.footprint);
        touched.extend(missed.keys().cloned());
        for key in touched {
            let row = rows.entry(key.clone()).or_default();
            row.tally.denominator += 1;
            if let Some(events) = missed.get(&key) {
                row.tally.numerator += 1;
                row.widenings += events;
            }
        }
    }
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(path, row)| {
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({ "path": path, "widenings": row.widenings }),
            )
        })
        .collect();
    json!({ "rows": rows, "below_support": below_support, "population": population })
}

/// Path pairs reserved together: Jaccard overlap of the reservations naming
/// each, `both ÷ either`. Reservations above [`LARGE_FOOTPRINT_CAP`] files
/// are excluded; pairs reserved together fewer than `min_support` times are
/// omitted.
fn change_coupling(facts: &CodebaseFacts, params: &Params) -> Value {
    let mut singles: BTreeMap<String, usize> = BTreeMap::new();
    let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut counted = 0;
    let mut excluded_large = 0;
    for reservation in &facts.reservations {
        if !params.within(reservation.at) {
            continue;
        }
        if reservation.files.len() > LARGE_FOOTPRINT_CAP {
            excluded_large += 1;
            continue;
        }
        counted += 1;
        let keys: Vec<String> = params.rollup.keys(&reservation.files).into_iter().collect();
        for (index, left) in keys.iter().enumerate() {
            *singles.entry(left.clone()).or_default() += 1;
            for right in &keys[index + 1..] {
                *pairs.entry((left.clone(), right.clone())).or_default() += 1;
            }
        }
    }
    let mut below_support = 0;
    let mut rows: Vec<(String, String, usize, usize, usize, usize)> = pairs
        .into_iter()
        .filter_map(|((left, right), both)| {
            if both < params.min_support {
                below_support += 1;
                return None;
            }
            let left_count = singles.get(&left).copied().unwrap_or(both);
            let right_count = singles.get(&right).copied().unwrap_or(both);
            let either = left_count + right_count - both;
            Some((left, right, both, either, left_count, right_count))
        })
        .collect();
    rows.sort_by(|left, right| {
        rate(right.2, right.3)
            .total_cmp(&rate(left.2, left.3))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| (&left.0, &left.1).cmp(&(&right.0, &right.1)))
    });
    rows.truncate(ROW_LIMIT);
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|(left, right, both, either, left_count, right_count)| {
            merge(
                rate_fields(both, either),
                json!({
                    "paths": [left, right],
                    "reservations": [left_count, right_count],
                }),
            )
        })
        .collect();
    json!({
        "rows": rows,
        "below_support": below_support,
        "reservations": counted,
        "excluded_large": excluded_large,
        "large_footprint_cap": LARGE_FOOTPRINT_CAP,
    })
}

/// Tasks that entered `blocked` in the window. A block the run failure
/// classification attributes to infrastructure is reported under its cause,
/// never against the task's files; a block nothing classifies is reported
/// in the `unclassified` bucket and its own per-path column. Only run
/// failures on the work count against the footprint: `blocked ÷ touched`.
fn blocked(facts: &CodebaseFacts, params: &Params) -> Value {
    #[derive(Default)]
    struct Row {
        tally: Tally,
        unclassified: usize,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut causes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut attributed = 0;
    let mut unclassified = 0;
    for task in &facts.tasks {
        let blocks: Vec<BlockCause> = task
            .blocks
            .iter()
            .filter(|(at, _)| params.within(*at))
            .map(|(_, cause)| *cause)
            .collect();
        if blocks.is_empty() && !params.within(task.updated_at) {
            continue;
        }
        let task_causes: BTreeSet<&'static str> = blocks
            .iter()
            .filter_map(|cause| match cause {
                BlockCause::Infra(name) => Some(*name),
                _ => None,
            })
            .collect();
        for cause in task_causes {
            *causes.entry(cause).or_default() += 1;
        }
        let run = blocks.contains(&BlockCause::Run);
        let unknown = !run && blocks.contains(&BlockCause::Unclassified);
        attributed += usize::from(run);
        unclassified += usize::from(unknown);
        for key in params.rollup.keys(&task.footprint) {
            let row = rows.entry(key).or_default();
            row.tally.denominator += 1;
            row.tally.numerator += usize::from(run);
            row.unclassified += usize::from(unknown);
        }
    }
    // A path with only unclassified blocks still earns a row, so that bucket
    // stays visible per path; it ranks by its attributed rate of zero.
    let (ranked, below_support) = rank_rows(
        rows,
        params.min_support,
        &|row: &Row| (row.tally.numerator, row.tally.denominator),
        |row| row.tally.numerator > 0 || row.unclassified > 0,
    );
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(path, row)| {
            merge(
                rate_fields(row.tally.numerator, row.tally.denominator),
                json!({ "path": path, "unclassified": row.unclassified }),
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
    cause_rows
        .push(json!({ "cause": "unclassified", "kind": "unclassified", "tasks": unclassified }));
    cause_rows.push(json!({ "cause": "run_failure", "kind": "attributed", "tasks": attributed }));
    json!({ "rows": rows, "below_support": below_support, "causes": cause_rows })
}

/// Rejected tasks that had a job run ÷ tasks with a run whose footprint
/// touched the path. Proposal-stage rejections (no run) are excluded.
fn rejected(facts: &CodebaseFacts, params: &Params) -> Value {
    let mut rows: BTreeMap<String, Tally> = BTreeMap::new();
    let mut excluded_without_run = 0;
    let mut counted = 0;
    for task in &facts.tasks {
        let rejected_in_window = task.status == TaskStatus::Rejected
            && task.rejected_at.is_some_and(|at| params.within(at));
        if !task.had_run {
            excluded_without_run += usize::from(rejected_in_window);
            continue;
        }
        if !rejected_in_window && !params.within(task.updated_at) {
            continue;
        }
        counted += usize::from(rejected_in_window);
        for key in params.rollup.keys(&task.footprint) {
            let row = rows.entry(key).or_default();
            row.denominator += 1;
            row.numerator += usize::from(rejected_in_window);
        }
    }
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.numerator, row.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(path, tally)| {
            merge(
                rate_fields(tally.numerator, tally.denominator),
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
