//! The six rankings of the codebase view, computed from scan facts for one
//! window, minimum support and rollup, and the paths several of them share.
//!
//! Every ranking is a rate over distinct tasks (or requesting identities for
//! contention, reservations for coupling) with its numerator and denominator,
//! so a popular file does not lead merely because many tasks touch it. Rows whose denominator is below
//! the minimum support are omitted and counted in `below_support`.
//!
//! Each ranking's rows come from one builder that also keeps what stands
//! behind every numerator, so the per-path detail reads the counts the
//! rankings rank.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::facts::{
    ACTOR_KINDS, Anchor, Block, BlockCause, CodebaseFacts, INFRA_CAUSES, ReservationFacts,
    TaskFacts,
};

/// Rows returned per ranking.
pub(super) const ROW_LIMIT: usize = 25;
/// Regressed tasks listed in one regression row's drill-down.
const DRILLDOWN_LIMIT: usize = 20;
/// Reservations naming more files than this are left out of change coupling,
/// so a sweeping reservation does not pair everything with everything.
pub(super) const LARGE_FOOTPRINT_CAP: usize = 12;
/// The four rankings keyed by one path, in the order the overlap lists them.
pub(super) const PATH_RANKINGS: [&str; 4] =
    ["regressions", "footprint_misses", "blocked", "rejected"];

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
pub(super) struct Tally {
    pub(super) numerator: usize,
    pub(super) denominator: usize,
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
/// Every kept row is returned: a section shows the first [`ROW_LIMIT`], and
/// the per-path detail reports a path's place in the whole order.
pub(super) fn rank<T>(
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
    (kept, below_support)
}

pub(super) fn rate_fields(numerator: usize, denominator: usize) -> Value {
    json!({
        "rate": rate(numerator, denominator),
        "numerator": numerator,
        "denominator": denominator,
    })
}

pub(super) fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

pub(super) fn all_sections(facts: &CodebaseFacts, params: &Params) -> Value {
    let mut sections = json!({
        "regressions": regressions(facts, params),
        "lock_contention": lock_contention(facts, params),
        "footprint_misses": footprint_misses(facts, params),
        "change_coupling": change_coupling(facts, params),
        "blocked": blocked(facts, params),
        "rejected": rejected(facts, params),
    });
    sections["overlap"] = overlap(&sections);
    sections
}

/// Paths in two or more of the [`PATH_RANKINGS`] as listed (each ranking's
/// first [`ROW_LIMIT`] rows): by how many, then by the highest rate outside
/// Rejected (whose one-off rejections would otherwise lead), then by path.
/// `ranked_paths` counts the distinct paths across the four lists.
fn overlap(sections: &Value) -> Value {
    let mut index: BTreeMap<&str, Vec<(&str, f64)>> = BTreeMap::new();
    for ranking in PATH_RANKINGS {
        for row in sections[ranking]["rows"].as_array().into_iter().flatten() {
            if let Some(path) = row["path"].as_str() {
                let rate = row["rate"].as_f64().unwrap_or(0.0);
                index.entry(path).or_default().push((ranking, rate));
            }
        }
    }
    let ranked_paths = index.len();
    let mut rows: Vec<(&str, Vec<&str>, f64)> = index
        .into_iter()
        .filter(|(_, hits)| hits.len() >= 2)
        .map(|(path, hits)| {
            let top = hits
                .iter()
                .filter(|(ranking, _)| *ranking != "rejected")
                .map(|(_, rate)| *rate)
                .fold(0.0, f64::max);
            let rankings = hits.into_iter().map(|(ranking, _)| ranking).collect();
            (path, rankings, top)
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| right.2.total_cmp(&left.2))
            .then_with(|| left.0.cmp(right.0))
    });
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|(path, rankings, top)| json!({ "path": path, "rankings": rankings, "top_rate": top }))
        .collect();
    json!({ "rows": rows, "ranked_paths": ranked_paths })
}

#[derive(Default)]
pub(super) struct RegressionRow<'a> {
    pub(super) tally: Tally,
    /// The regressed tasks: the numerator.
    pub(super) regressed: Vec<&'a TaskFacts>,
    by_crew: BTreeMap<String, Tally>,
}

/// Distinct done tasks with an incoming `regression_from` ÷ done tasks whose
/// footprint touched the path, for tasks that became done in the window.
/// Also returns the number of done tasks in the window.
pub(super) fn regression_rows<'a>(
    facts: &'a CodebaseFacts,
    params: &Params,
) -> (BTreeMap<String, RegressionRow<'a>>, usize) {
    let mut rows: BTreeMap<String, RegressionRow> = BTreeMap::new();
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
    (rows, population)
}

fn regressions(facts: &CodebaseFacts, params: &Params) -> Value {
    let (rows, population) = regression_rows(facts, params);
    let (ranked, below_support) = rank(rows, params.min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
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

#[derive(Default)]
pub(super) struct LockRow {
    /// The requesters refused a lock on the path: the numerator.
    pub(super) contenders: BTreeSet<String>,
    pub(super) requesters: BTreeSet<String>,
    pub(super) reservations: usize,
    pub(super) denials: usize,
}

pub(super) struct LockRows {
    pub(super) rows: BTreeMap<String, LockRow>,
    reservations: usize,
    denials: usize,
}

/// Distinct requesters that waited on a path because of a lock conflict ÷
/// distinct requesters that asked for it (reserved it or were refused it). A retry
/// loop repeats the same contender and counts once. Reservation and denial
/// counts are reported beside, not folded into, the contention figure, per
/// path and as window totals.
pub(super) fn lock_rows(facts: &CodebaseFacts, params: &Params) -> LockRows {
    let mut rows: BTreeMap<String, LockRow> = BTreeMap::new();
    let mut reservations = 0;
    let mut denials = 0;
    for reservation in &facts.reservations {
        if !params.within(reservation.at) {
            continue;
        }
        reservations += 1;
        for key in params.rollup.keys(&reservation.files) {
            let row = rows.entry(key).or_default();
            row.reservations += 1;
            row.requesters
                .extend(reservation.requesters.iter().cloned());
        }
    }
    for denial in &facts.lock_denials {
        if !params.within(denial.at) {
            continue;
        }
        denials += 1;
        for key in params.rollup.keys(&denial.files) {
            let row = rows.entry(key).or_default();
            row.denials += 1;
            row.contenders.extend(denial.contenders.iter().cloned());
            row.requesters.extend(denial.contenders.iter().cloned());
        }
    }
    LockRows {
        rows,
        reservations,
        denials,
    }
}

pub(super) fn lock_tally(row: &LockRow) -> (usize, usize) {
    (row.contenders.len(), row.requesters.len())
}

fn lock_contention(facts: &CodebaseFacts, params: &Params) -> Value {
    let LockRows {
        rows,
        reservations,
        denials,
    } = lock_rows(facts, params);
    let (ranked, below_support) = rank(rows, params.min_support, lock_tally);
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
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
        "reservations": reservations,
        "denials": denials,
        "wait_time_recorded": false,
    })
}

#[derive(Default)]
pub(super) struct FootprintRow<'a> {
    pub(super) tally: Tally,
    pub(super) widenings: usize,
    /// Each task that widened onto the path, with its latest such widening
    /// in the window: the numerator.
    pub(super) missed: Vec<(&'a TaskFacts, DateTime<Utc>)>,
}

/// Distinct tasks that had the path added mid-run by `context_files_widened`
/// ÷ tasks whose footprint touched it, for tasks active in the window.
/// Also returns the number of tasks active in the window.
pub(super) fn footprint_rows<'a>(
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

fn footprint_misses(facts: &CodebaseFacts, params: &Params) -> Value {
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

/// One ranked pair: its paths, the reservations holding both (the
/// numerator), how many held either, and how many held each path.
pub(super) struct CouplingPair<'a> {
    pub(super) paths: (String, String),
    pub(super) both: Vec<&'a ReservationFacts>,
    pub(super) either: usize,
    pub(super) counts: (usize, usize),
}

pub(super) struct CouplingPairs<'a> {
    /// Pairs at or above the minimum support, in ranked order.
    pub(super) ranked: Vec<CouplingPair<'a>>,
    /// Reservations naming each path, over the counted reservations.
    pub(super) singles: BTreeMap<String, usize>,
    below_support: usize,
    counted: usize,
    excluded_large: usize,
}

/// Path pairs reserved together: Jaccard overlap of the reservations naming
/// each, `both ÷ either`. Reservations above [`LARGE_FOOTPRINT_CAP`] files
/// are excluded; pairs reserved together fewer than `min_support` times are
/// omitted.
pub(super) fn coupling_pairs<'a>(facts: &'a CodebaseFacts, params: &Params) -> CouplingPairs<'a> {
    let mut singles: BTreeMap<String, usize> = BTreeMap::new();
    let mut pairs: BTreeMap<(String, String), Vec<&ReservationFacts>> = BTreeMap::new();
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
                pairs
                    .entry((left.clone(), right.clone()))
                    .or_default()
                    .push(reservation);
            }
        }
    }
    let mut below_support = 0;
    let mut ranked: Vec<CouplingPair> = pairs
        .into_iter()
        .filter_map(|((left, right), both)| {
            if both.len() < params.min_support {
                below_support += 1;
                return None;
            }
            let left_count = singles.get(&left).copied().unwrap_or(both.len());
            let right_count = singles.get(&right).copied().unwrap_or(both.len());
            let either = left_count + right_count - both.len();
            Some(CouplingPair {
                paths: (left, right),
                both,
                either,
                counts: (left_count, right_count),
            })
        })
        .collect();
    ranked.sort_by(|left, right| {
        rate(right.both.len(), right.either)
            .total_cmp(&rate(left.both.len(), left.either))
            .then_with(|| right.both.len().cmp(&left.both.len()))
            .then_with(|| left.paths.cmp(&right.paths))
    });
    CouplingPairs {
        ranked,
        singles,
        below_support,
        counted,
        excluded_large,
    }
}

fn change_coupling(facts: &CodebaseFacts, params: &Params) -> Value {
    let CouplingPairs {
        ranked,
        below_support,
        counted,
        excluded_large,
        ..
    } = coupling_pairs(facts, params);
    let rows: Vec<Value> = ranked
        .into_iter()
        .take(ROW_LIMIT)
        .map(|pair| {
            let (left, right) = pair.paths;
            merge(
                rate_fields(pair.both.len(), pair.either),
                json!({
                    "paths": [left, right],
                    "reservations": [pair.counts.0, pair.counts.1],
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

#[derive(Default)]
pub(super) struct BlockedRow<'a> {
    pub(super) tally: Tally,
    /// Each task a run failure blocked, or a base conflict blocked on this
    /// path, with its latest such block in the window: the numerator.
    pub(super) blocked: Vec<(&'a TaskFacts, DateTime<Utc>)>,
    /// Each task blocked in the window with no cause found and no run
    /// failure, with its latest such block.
    pub(super) unclassified: Vec<(&'a TaskFacts, DateTime<Utc>)>,
}

pub(super) struct BlockedRows<'a> {
    pub(super) rows: BTreeMap<String, BlockedRow<'a>>,
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
pub(super) fn blocked_rows<'a>(facts: &'a CodebaseFacts, params: &Params) -> BlockedRows<'a> {
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
pub(super) fn rank_blocked(
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

fn blocked(facts: &CodebaseFacts, params: &Params) -> Value {
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

#[derive(Default)]
pub(super) struct RejectedRow<'a> {
    pub(super) tally: Tally,
    /// Each task rejected after a run in the window, with when: the
    /// numerator.
    pub(super) rejected: Vec<(&'a TaskFacts, DateTime<Utc>)>,
}

pub(super) struct RejectedRows<'a> {
    pub(super) rows: BTreeMap<String, RejectedRow<'a>>,
    counted: usize,
    excluded_without_run: usize,
}

/// Rejected tasks that had a job run ÷ tasks with a run whose footprint
/// touched the path. Proposal-stage rejections (no run) are excluded.
pub(super) fn rejected_rows<'a>(facts: &'a CodebaseFacts, params: &Params) -> RejectedRows<'a> {
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

fn rejected(facts: &CodebaseFacts, params: &Params) -> Value {
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
