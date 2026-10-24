//! Pilots: the crew and model of the task-pilot run behind each task's
//! newest assessment, graded on the done tasks it assessed against the files
//! their landed commit changed.
//!
//! The unit is an assessed task: a done task whose newest pilot receipt was
//! written in the window, bulk and `no-diff-expected` tasks left out (see
//! [`crate::application::codebase::pilot_evidence`]). Recall, precision and
//! F1 grade the pilot's final selectors, and the filer's selectors from
//! before any pilot ran are the baseline the F1 lift is measured from. The
//! observed tier ranks the window's assessed tasks by total implementation
//! cost and cuts the ranking in the proportions the pilots assigned, so
//! calibration measures ordering, not absolute cost. Cost is never a term.
//!
//! The warning terms grade every assessment, held or clean, by how it
//! settled ([`super::pilot_warnings`]), rated per 100 settled assessments
//! rather than per assessed task.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::fs::path_glob::RelativePathGlob;
use orbit_types::task::TaskComplexity;
use serde_json::{Value, json};

use super::RoleSpec;
use super::pilot_warnings::{self, CONFIRMED, DISMISSED, MISSED, SETTLED_UNIT, WarningRow};
use crate::application::agents::AgentFacts;
use crate::application::agents::attribution::row_key;
use crate::application::agents::facts::TaskFacts;
use crate::application::agents::score::{Occurrence, RowInput, Stat, Tally, Term, number};
use crate::application::codebase::{Anchor, Contest, HeldWait, PilotEvidence, PilotRun};

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "assessed tasks",
    terms: TERMS,
    term_units: &[
        (CONFIRMED, SETTLED_UNIT),
        (DISMISSED, SETTLED_UNIT),
        (MISSED, SETTLED_UNIT),
    ],
    rows: Some(rows),
    header: Some(pilot_warnings::header),
};

const TERMS: &[Term] = &[
    Term {
        key: "f1_lift",
        label: "F1 lift",
        weight: 1.0,
        definition: "F1 points gained over the filer's own selectors from before any pilot ran, per task.",
        group: "credit",
    },
    Term {
        key: "tier_calibration",
        label: "Tier calibration",
        weight: 0.25,
        definition: "Tasks whose assessed complexity matches their observed tier, so per 100 tasks the percent on the tier.",
        group: "credit",
    },
    Term {
        key: CONFIRMED,
        label: "Confirmed warnings",
        weight: 3.0,
        definition: "Held assessments proved right: before approval someone other than the pilot edited the description or acceptance criteria, or the task was rejected or archived; or an unedited approval's delivery then blocked or was rejected.",
        group: "credit",
    },
    Term {
        key: DISMISSED,
        label: "Dismissed warnings",
        weight: -1.0,
        definition: "Held assessments overridden as harmless: approved with no field edit by anyone other than the pilot, then done with no block.",
        group: "problem",
    },
    Term {
        key: MISSED,
        label: "Missed scope blocks",
        weight: -3.0,
        definition: "Clean assessments whose task was promoted, then blocked on a decision blocked-task triage proposed. Reads 0 while no triage record is in the window; the payload's sources say so.",
        group: "problem",
    },
    Term {
        key: "costly_misses",
        label: "Costly misses",
        weight: -1.0,
        definition: "Landed files no pilot selector covered that a base-conflict block or a refused lock request of the task named.",
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

/// The tiers a pilot assigns, cheapest first; `unassessed` is no tier.
const TIERS: [TaskComplexity; 4] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
    TaskComplexity::XHard,
];

/// Sources whose CLI help, CLI output and MCP tool goldens are regenerated
/// from: a golden counts as covered when a selector reaches one of them.
const CLI_MCP_SOURCES: &[&str] = &[
    "crates/orbit-cli/src/",
    "crates/orbit-mcp/src/",
    "crates/orbit-tools/src/",
    "crates/orbit-common/src/governance/",
];

/// Where a generated or mirrored output comes from.
enum Source {
    /// The same relative path under another prefix.
    Mirror(&'static str),
    /// Any file under these prefixes.
    Within(&'static [&'static str]),
}

/// Generated or mirrored outputs, by path prefix (a trailing `/`) or exact
/// path. A landed output counts as covered when a selector covers its source.
const DERIVED: &[(&str, Source)] = &[
    (
        "plugin/skills/",
        Source::Mirror("crates/orbit-core/assets/skills/"),
    ),
    (
        "crates/orbit-cli/tests/help_goldens/",
        Source::Within(CLI_MCP_SOURCES),
    ),
    (
        "crates/orbit-cli/tests/output_goldens/",
        Source::Within(CLI_MCP_SOURCES),
    ),
    (
        "crates/orbit-cli/tests/snapshots/",
        Source::Within(CLI_MCP_SOURCES),
    ),
    ("docs/INDEX.md", Source::Within(&["docs/"])),
];

/// One declared selector that can cover files.
struct Declared<'a> {
    raw: &'a str,
    anchor: Anchor,
    /// Set for a selector whose path holds a wildcard.
    glob: Option<RelativePathGlob>,
}

/// The path selectors among `raw`, one per anchor. Module and command
/// selectors name no files and are left out.
fn declared(raw: &[String]) -> Vec<Declared<'_>> {
    let mut seen = BTreeSet::new();
    raw.iter()
        .filter_map(|raw| {
            let anchor = Anchor::parse(raw)?;
            if !seen.insert(anchor.clone()) {
                return None;
            }
            let glob = anchor
                .path
                .contains(['*', '?'])
                .then(|| {
                    let pattern = if anchor.dir {
                        format!("{}/**", anchor.path)
                    } else {
                        anchor.path.clone()
                    };
                    RelativePathGlob::new(&pattern).ok()
                })
                .flatten();
            Some(Declared { raw, anchor, glob })
        })
        .collect()
}

impl Declared<'_> {
    /// A file selector covers its file, a directory selector the files
    /// beneath it, and a wildcard selector the files its pattern matches.
    fn covers(&self, path: &str) -> bool {
        if let Some(glob) = &self.glob {
            return glob.matches(path);
        }
        self.anchor.path == path
            || (self.anchor.dir
                && path
                    .strip_prefix(&self.anchor.path)
                    .is_some_and(|rest| rest.starts_with('/')))
    }

    /// Whether the selector reaches any file under `prefix`.
    fn reaches(&self, prefix: &str) -> bool {
        self.anchor.path.starts_with(prefix)
            || (self.anchor.dir && prefix.starts_with(&format!("{}/", self.anchor.path)))
    }

    /// [`Self::covers`], or covers the source a generated output comes from.
    fn covers_landed(&self, path: &str) -> bool {
        self.covers(path)
            || DERIVED.iter().any(|(output, source)| {
                let rest = if output.ends_with('/') {
                    path.strip_prefix(output)
                } else {
                    (path == *output).then_some("")
                };
                rest.is_some_and(|rest| match source {
                    Source::Mirror(prefix) => self.covers(&format!("{prefix}{rest}")),
                    Source::Within(prefixes) => prefixes.iter().any(|prefix| self.reaches(prefix)),
                })
            })
    }

    /// Whether a lock through this selector holds `path`: the selector
    /// covers it, or `path` is a directory holding the selector's path.
    fn reserves(&self, path: &Anchor) -> bool {
        self.covers(&path.path)
            || (path.dir
                && self
                    .anchor
                    .path
                    .strip_prefix(&path.path)
                    .is_some_and(|rest| rest.starts_with('/')))
    }
}

/// Sums behind recall and precision over one or more tasks.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::application::agents) struct Score {
    /// Landed files some selector covered.
    covered: usize,
    landed: usize,
    /// Selectors that covered at least one landed file.
    useful: usize,
    declared: usize,
}

impl Score {
    fn add(&mut self, other: &Score) {
        self.covered += other.covered;
        self.landed += other.landed;
        self.useful += other.useful;
        self.declared += other.declared;
    }

    fn recall(&self) -> f64 {
        ratio(self.covered, self.landed)
    }

    fn precision(&self) -> f64 {
        ratio(self.useful, self.declared)
    }

    /// The harmonic mean of precision and recall; 0 when both are.
    fn f1(&self) -> f64 {
        let (precision, recall) = (self.precision(), self.recall());
        if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        }
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Grade `selectors` against a landed diff: the score, the landed files no
/// selector covered, and each path selector with whether it covered one.
fn grade<'a>(
    selectors: &'a [String],
    landed: &BTreeSet<String>,
) -> (Score, Vec<String>, Vec<(Declared<'a>, bool)>) {
    let mut declared: Vec<(Declared<'a>, bool)> = declared(selectors)
        .into_iter()
        .map(|selector| (selector, false))
        .collect();
    let mut missed = Vec::new();
    for path in landed {
        let mut covered = false;
        for (selector, useful) in &mut declared {
            if selector.covers_landed(path) {
                *useful = true;
                covered = true;
            }
        }
        if !covered {
            missed.push(path.clone());
        }
    }
    let score = Score {
        covered: landed.len() - missed.len(),
        landed: landed.len(),
        useful: declared.iter().filter(|(_, useful)| *useful).count(),
        declared: declared.len(),
    };
    (score, missed, declared)
}

/// One assessed task, graded once per scan.
#[derive(Debug, Clone)]
pub(in crate::application::agents) struct Graded {
    pub(in crate::application::agents) task_id: String,
    /// When the newest assessment was written: its window.
    pub(in crate::application::agents) at: DateTime<Utc>,
    pub(in crate::application::agents) run: Option<PilotRun>,
    pilot: Score,
    author: Score,
    /// Landed files no pilot selector covered.
    missed: Vec<String>,
    /// Pilot selectors that covered no landed file.
    unused: Vec<String>,
    /// Missed landed files the task's work was contested on, each once with
    /// its first contest.
    pub(in crate::application::agents) costly: Vec<(String, Contest)>,
    /// Waits others spent on paths the task held only through pilot
    /// selectors that covered none of its landed files.
    pub(in crate::application::agents) waits: Vec<HeldWait>,
}

impl Graded {
    /// The row the assessment counts in.
    pub(in crate::application::agents) fn key(&self) -> (String, String) {
        let run = self.run.as_ref();
        row_key(
            run.and_then(|run| run.crew.as_deref()),
            run.and_then(|run| run.model.as_deref()),
        )
    }

    pub(in crate::application::agents) fn run_id(&self) -> Option<String> {
        self.run.as_ref().map(|run| run.run_id.clone())
    }

    fn lift(&self) -> f64 {
        self.pilot.f1() - self.author.f1()
    }
}

/// Grade every assessment the scan found and decide which of its contests
/// and held waits count against its pilot.
pub(in crate::application::agents) fn grade_all(evidence: PilotEvidence) -> Vec<Graded> {
    let mut waits: BTreeMap<String, Vec<HeldWait>> = BTreeMap::new();
    for wait in evidence.waits {
        waits.entry(wait.holder.clone()).or_default().push(wait);
    }
    evidence
        .assessments
        .into_iter()
        .map(|assessment| {
            let (pilot, missed, selectors) = grade(&assessment.after, &assessment.landed);
            let unused = selectors
                .iter()
                .filter(|(_, useful)| !*useful)
                .map(|(selector, _)| selector.raw.to_string())
                .collect();
            let mut contested = assessment.contested.clone();
            contested.sort_by_key(|contest| contest.at);
            let costly = missed
                .iter()
                .filter_map(|path| {
                    let contest = contested.iter().find(|contest| {
                        contest.path.path == *path
                            || (contest.path.dir
                                && path
                                    .strip_prefix(&contest.path.path)
                                    .is_some_and(|rest| rest.starts_with('/')))
                    })?;
                    Some((path.clone(), contest.clone()))
                })
                .collect();
            let held = waits
                .remove(&assessment.task_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|wait| {
                    let mut reserving = selectors
                        .iter()
                        .filter(|(selector, _)| selector.reserves(&wait.path))
                        .peekable();
                    reserving.peek().is_some() && reserving.all(|(_, useful)| !*useful)
                })
                .collect();
            let (author, _, _) = grade(&assessment.author, &assessment.landed);
            Graded {
                task_id: assessment.task_id,
                at: assessment.at,
                run: assessment.run,
                pilot,
                author,
                missed,
                unused,
                costly,
                waits: held,
            }
        })
        .collect()
}

type Key = (String, String);

/// The tier a task was assessed at, as an index into [`TIERS`].
fn tier_index(complexity: Option<TaskComplexity>) -> Option<usize> {
    complexity.and_then(|complexity| TIERS.iter().position(|tier| *tier == complexity))
}

/// A task's total implementation cost: every attempt's run, each priced.
/// `None` when an attempt names no run or a run has no recorded cost.
fn implementation_cost(facts: &AgentFacts, task: &TaskFacts) -> Option<f64> {
    if task.attempts.is_empty() {
        return None;
    }
    let runs: BTreeSet<&str> = task
        .attempts
        .iter()
        .map(|attempt| attempt.runner.run_id.as_deref())
        .collect::<Option<_>>()?;
    runs.iter().try_fold(0.0, |total, id| {
        facts
            .run_costs
            .get(*id)
            .copied()
            .flatten()
            .map(|cost| total + cost)
    })
}

/// Each placed task's assessed and observed tier: the window's assessed
/// tasks with a tier and a cost, cheapest first, cut into tiers in the
/// proportions they were assessed at.
fn observed_tiers<'a>(
    facts: &AgentFacts,
    assessed: &[(&'a Graded, &'a TaskFacts)],
) -> BTreeMap<&'a str, (usize, usize)> {
    let mut placed: Vec<(&str, usize, f64)> = assessed
        .iter()
        .filter_map(|(graded, task)| {
            Some((
                graded.task_id.as_str(),
                tier_index(task.complexity)?,
                implementation_cost(facts, task)?,
            ))
        })
        .collect();
    let mut proportions = [0usize; TIERS.len()];
    for (_, tier, _) in &placed {
        proportions[*tier] += 1;
    }
    placed.sort_by(|left, right| {
        left.2
            .partial_cmp(&right.2)
            .unwrap_or(Ordering::Equal)
            .then(left.0.cmp(right.0))
    });
    let mut observed = proportions
        .iter()
        .enumerate()
        .flat_map(|(tier, count)| std::iter::repeat_n(tier, *count));
    placed
        .into_iter()
        .filter_map(|(id, assessed, _)| Some((id, (assessed, observed.next()?))))
        .collect()
}

/// One row per pilot crew and model over the assessments written in the
/// window.
fn rows(facts: &AgentFacts, since: DateTime<Utc>) -> Vec<RowInput<'_>> {
    let tasks: BTreeMap<&str, &TaskFacts> = facts
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect();
    let assessed: Vec<(&Graded, &TaskFacts)> = facts
        .pilots
        .iter()
        .filter(|graded| graded.at >= since)
        .filter_map(|graded| Some((graded, *tasks.get(graded.task_id.as_str())?)))
        .collect();
    let tiers = observed_tiers(facts, &assessed);
    let present: Vec<usize> = (0..TIERS.len())
        .filter(|tier| {
            tiers
                .values()
                .any(|(assessed, observed)| assessed == tier || observed == tier)
        })
        .collect();
    let mut work: BTreeMap<Key, Vec<(&Graded, &TaskFacts)>> = BTreeMap::new();
    for (graded, task) in assessed {
        work.entry(graded.key()).or_default().push((graded, task));
    }
    let mut warnings = pilot_warnings::rows(facts, since);
    for key in warnings.keys() {
        work.entry(key.clone()).or_default();
    }
    work.into_iter()
        .map(|(row, assessed)| {
            let warnings = warnings.remove(&row).unwrap_or_default();
            row_input(facts, row, &assessed, &tiers, &present, warnings)
        })
        .collect()
}

fn row_input<'a>(
    facts: &AgentFacts,
    row: Key,
    assessed: &[(&'a Graded, &'a TaskFacts)],
    tiers: &BTreeMap<&str, (usize, usize)>,
    present: &[usize],
    warnings: WarningRow<'a>,
) -> RowInput<'a> {
    let denominators = warnings.denominators();
    let warning_stats = warnings.stats();
    let mut tallies: BTreeMap<&'static str, Tally<'a>> = warnings.tallies;
    let mut pilot = Score::default();
    let mut author = Score::default();
    let mut matrix = vec![vec![0usize; TIERS.len()]; TIERS.len()];
    let mut runs: BTreeMap<String, Option<f64>> = BTreeMap::new();
    let mut runs_without_model = 0;
    for &(graded, task) in assessed {
        pilot.add(&graded.pilot);
        author.add(&graded.author);
        if let Some(run) = &graded.run {
            runs.insert(run.run_id.clone(), run.seconds);
        }
        runs_without_model +=
            usize::from(graded.run.as_ref().is_none_or(|run| run.model.is_none()));
        let lift = graded.lift();
        if lift != 0.0 {
            let mut detail = format!(
                "pilot F1 {} against the filer's {}",
                percent(graded.pilot.f1()),
                percent(graded.author.f1())
            );
            if !graded.missed.is_empty() {
                detail.push_str(&format!("; missed {}", graded.missed.join(", ")));
            }
            if !graded.unused.is_empty() {
                detail.push_str(&format!("; unused {}", graded.unused.join(", ")));
            }
            tallies.entry("f1_lift").or_default().add(
                task,
                lift,
                Some(Occurrence {
                    at: graded.at,
                    detail,
                    run_id: graded.run_id(),
                }),
            );
        }
        if let Some(&(assessed_tier, observed_tier)) = tiers.get(graded.task_id.as_str()) {
            matrix[assessed_tier][observed_tier] += 1;
            if assessed_tier == observed_tier {
                tallies.entry("tier_calibration").or_default().add(
                    task,
                    1.0,
                    Some(Occurrence {
                        at: graded.at,
                        detail: format!("assessed and observed {}", TIERS[assessed_tier].as_str()),
                        run_id: graded.run_id(),
                    }),
                );
            }
        }
        for (path, contest) in &graded.costly {
            tallies.entry("costly_misses").or_default().add(
                task,
                1.0,
                Some(Occurrence {
                    at: contest.at,
                    detail: costly_detail(path, contest),
                    run_id: graded.run_id(),
                }),
            );
        }
        for wait in &graded.waits {
            tallies.entry("lock_wait_hours").or_default().add(
                task,
                wait.wait_ms as f64 / 3_600_000.0,
                Some(Occurrence {
                    at: wait.started_at,
                    detail: wait_detail(wait),
                    run_id: graded.run_id(),
                }),
            );
        }
    }
    let units = assessed.len();
    // Unpriced when an assessment has no run or a run has no recorded cost.
    let cost = if runs.is_empty() || assessed.iter().any(|(graded, _)| graded.run.is_none()) {
        None
    } else {
        runs.keys().try_fold(0.0, |total, id| {
            facts
                .run_costs
                .get(id)
                .copied()
                .flatten()
                .map(|cost| total + cost)
        })
    };
    let mut durations: Vec<f64> = runs.values().filter_map(|seconds| *seconds).collect();
    durations.sort_by(f64::total_cmp);
    let median = (!durations.is_empty()).then(|| {
        let middle = durations.len() / 2;
        (durations[middle] + durations[(durations.len() - 1) / 2]) / 2.0
    });
    let selectors: usize = assessed
        .iter()
        .map(|(graded, _)| graded.pilot.declared)
        .sum();
    let placed: usize = matrix.iter().flatten().sum();
    let mut on_tier = 0;
    let mut too_easy = 0;
    let mut too_hard = 0;
    let mut within_one = 0;
    for (assessed_tier, counts) in matrix.iter().enumerate() {
        for (observed_tier, count) in counts.iter().enumerate() {
            match assessed_tier.cmp(&observed_tier) {
                Ordering::Equal => on_tier += count,
                Ordering::Less => too_easy += count,
                Ordering::Greater => too_hard += count,
            }
            if assessed_tier.abs_diff(observed_tier) <= 1 {
                within_one += count;
            }
        }
    }
    let tier_matrix = json!({
        "tiers": present.iter().map(|tier| TIERS[*tier].as_str()).collect::<Vec<_>>(),
        "rows": present.iter().map(|assessed_tier| json!({
            "assessed": TIERS[*assessed_tier].as_str(),
            "observed": present.iter().map(|observed_tier| matrix[*assessed_tier][*observed_tier]).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "placed": placed,
        "not_placed": units - placed,
        "on_tier": on_tier,
        "too_easy": too_easy,
        "too_hard": too_hard,
    });
    RowInput {
        crew: row.0,
        model: row.1,
        units,
        runs_without_model,
        provisional: None,
        tallies,
        expected: None,
        denominators,
        stats: [
            stat(
                "recall_percent",
                "Recall (%)",
                number(pilot.recall() * 100.0),
            ),
            stat(
                "precision_percent",
                "Precision (%)",
                number(pilot.precision() * 100.0),
            ),
            stat("f1_percent", "F1 (%)", number(pilot.f1() * 100.0)),
            stat(
                "filer_f1_percent",
                "Filer's F1 (%)",
                number(author.f1() * 100.0),
            ),
            stat(
                "missed_files",
                "Landed files no pilot selector covered",
                json!(pilot.landed - pilot.covered),
            ),
            stat(
                "unused_selectors",
                "Pilot selectors that covered no landed file",
                json!(pilot.declared - pilot.useful),
            ),
            stat(
                "selectors_per_task",
                "Selectors per task",
                number(ratio(selectors, units)),
            ),
            stat(
                "within_one_tier_percent",
                "Assessed within one tier (%)",
                if placed == 0 {
                    Value::Null
                } else {
                    number(within_one as f64 * 100.0 / placed as f64)
                },
            ),
            stat(
                "cost_per_assessment",
                "Cost per assessment (USD)",
                cost.map(|cost| json!(cost / units as f64))
                    .unwrap_or(Value::Null),
            ),
            stat(
                "median_run_seconds",
                "Median pilot run (seconds)",
                median.map(number).unwrap_or(Value::Null),
            ),
            stat("tier_matrix", "Assessed against observed tier", tier_matrix),
        ]
        .into_iter()
        .chain(warning_stats)
        .collect(),
    }
}

fn percent(fraction: f64) -> String {
    format!("{}%", (fraction * 1000.0).round() / 10.0)
}

fn costly_detail(path: &str, contest: &Contest) -> String {
    format!("{path}: uncovered, then a {}", contest.kind)
}

fn wait_detail(wait: &HeldWait) -> String {
    format!(
        "{} waited {:.2} h on {}{} ({})",
        wait.waiter,
        wait.wait_ms as f64 / 3_600_000.0,
        wait.path.path,
        if wait.path.dir { "/" } else { "" },
        wait.end.replace('_', " "),
    )
}

/// The detail of a costly-miss charge.
pub(in crate::application::agents) fn costly_charge_detail(
    path: &str,
    contest: &Contest,
) -> String {
    costly_detail(path, contest)
}

/// The detail of a lock-wait charge.
pub(in crate::application::agents) fn wait_charge_detail(wait: &HeldWait) -> String {
    wait_detail(wait)
}

fn stat(key: &'static str, label: &'static str, value: Value) -> Stat {
    Stat { key, label, value }
}
