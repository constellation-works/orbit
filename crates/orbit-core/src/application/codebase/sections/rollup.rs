use super::*;

use super::blocked::blocked;
use super::coupling::change_coupling;
use super::footprint::footprint_misses;
use super::locks::lock_contention;
use super::regressions::regressions;
use super::rejected::rejected;

/// Rows returned per ranking.
pub(in crate::application::codebase) const ROW_LIMIT: usize = 25;

/// Regressed tasks, and sites, listed in one regression row's drill-down.
pub(super) const DRILLDOWN_LIMIT: usize = 20;

/// Reservations naming more files than this are left out of change coupling,
/// so a sweeping reservation does not pair everything with everything.
pub(in crate::application::codebase) const LARGE_FOOTPRINT_CAP: usize = 12;

/// The four rankings keyed by one path, in the order the overlap lists them.
pub(in crate::application::codebase) const PATH_RANKINGS: [&str; 4] =
    ["regressions", "footprint_misses", "blocked", "rejected"];

/// How footprint paths are grouped before counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::application::codebase) enum Rollup {
    File,
    Dir,
    Crate,
}

impl Rollup {
    pub(in crate::application::codebase) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "file" => Some(Self::File),
            "dir" => Some(Self::Dir),
            "crate" => Some(Self::Crate),
            _ => None,
        }
    }

    pub(in crate::application::codebase) fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Crate => "crate",
        }
    }

    /// The group an anchor counts under. Directories end in `/`; files at the
    /// repository root roll up to `./`.
    pub(super) fn key(self, anchor: &Anchor) -> String {
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

    pub(in crate::application::codebase) fn keys<'a>(
        self,
        anchors: impl IntoIterator<Item = &'a Anchor>,
    ) -> BTreeSet<String> {
        anchors.into_iter().map(|anchor| self.key(anchor)).collect()
    }
}

pub(in crate::application::codebase) struct Params {
    pub(in crate::application::codebase) since: Option<DateTime<Utc>>,
    pub(in crate::application::codebase) min_support: usize,
    pub(in crate::application::codebase) rollup: Rollup,
    /// The regressions the Regressions ranking counts.
    pub(in crate::application::codebase) filter: RegressionFilter,
}

impl Params {
    pub(in crate::application::codebase) fn within(&self, at: DateTime<Utc>) -> bool {
        self.since.is_none_or(|since| at >= since)
    }
}

/// A rate row before rendering: `numerator` of `denominator` distinct items.
#[derive(Default)]
pub(in crate::application::codebase) struct Tally {
    pub(in crate::application::codebase) numerator: usize,
    pub(in crate::application::codebase) denominator: usize,
}

pub(super) fn rate(numerator: usize, denominator: usize) -> f64 {
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
pub(in crate::application::codebase) fn rank<T>(
    rows: BTreeMap<String, T>,
    min_support: usize,
    tally: impl Fn(&T) -> (usize, usize),
) -> (Vec<(String, T)>, usize) {
    rank_rows(rows, min_support, &tally, |row| tally(row).0 > 0)
}

/// [`rank`] with an explicit test for whether a row has anything to show.
pub(super) fn rank_rows<T>(
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

pub(in crate::application::codebase) fn rate_fields(numerator: usize, denominator: usize) -> Value {
    json!({
        "rate": rate(numerator, denominator),
        "numerator": numerator,
        "denominator": denominator,
    })
}

pub(in crate::application::codebase) fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

pub(in crate::application::codebase) fn all_sections(
    facts: &CodebaseFacts,
    params: &Params,
) -> Value {
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
