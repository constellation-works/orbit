//! Reviewers: the crew and model of the review gate attempts, and of the
//! review jobs that file findings as tasks. Each settled review is one unit,
//! and so is each review job attempt that filed a finding. A reviewer earns
//! only for verdicts that turned out right: findings and fixes that held.
//! Its problems are the ones the one-owner rule gives reviewers. The accept
//! rate, the reject rate and the number of findings are shown, never scored.
//!
//! A review younger than the miss lookback is provisional: a `review_missed`
//! regression filed later still lowers the row that accepted the work.
//!
//! A filed finding is judged by finding verification's verdict, else by its
//! task's outcome: a landed task held, and a task dropped without landing is
//! neutral, since no typed record says why it was dropped. A row none of
//! whose units is judged (no gate settlement, and no filed finding with a
//! verdict or a terminal task) is unjudged: no score, no rank.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use orbit_types::task::{DefectSeverity, TaskComplexity};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use super::RoleSpec;
use crate::application::agents::attribution::{Owner, row_key};
use crate::application::agents::facts::{
    Finding, FindingState, RejectOutcome, Review, TaskFacts, filing_attempt,
};
use crate::application::agents::score::{Occurrence, RowInput, Stat, Tally, Term, number};
use crate::application::agents::{AgentFacts, AgentRole, Problem};
use crate::application::task::FindingVerdict;

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "settled reviews",
    terms: TERMS,
    term_units: &[],
    rows: Some(rows),
    header: None,
};

/// How long after a review a `review_missed` regression is still expected;
/// a review younger than this is provisional.
const MISS_LOOKBACK_DAYS: i64 = 30;
/// Accept-rate points above peers on the same tiers that tag a row
/// `accepts often`.
const ACCEPTS_OFTEN_POINTS: f64 = 10.0;
const TERMS: &[Term] = &[
    Term {
        key: "findings_held",
        label: "Findings that held",
        weight: 1.0,
        definition: "Findings repaired by the reviewer whose fixes passed owner revalidation, open findings on a reject whose next implementation run changed a path the finding cites, findings a review job filed that finding verification CONFIRMED or, with no verification verdict, whose task landed, and mock comparison findings (purpose visual_review) their in-line verifier CONFIRMED; each weighted by severity: critical 8, high 4, medium 2, low 1, nit 0.25. A CONFIRMED finding scores at the severity verification rated; an unverified landed finding at the highest severity rated on its own regression_from links, else low; a review gate finding at the reviewer's own. A filed finding whose task is still open is not judged yet.",
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
        definition: "Findings closed by an authorized decision (Disposed), findings a review job filed as tasks that finding verification REJECTED, and mock comparison findings (purpose visual_review) their in-line verifier REJECTED. A filed finding whose task was rejected or archived without landing and without a verification verdict costs nothing: task records carry no typed reason that tells a wrong finding from a duplicate, won't-fix or superseded one, so every such drop is neutral.",
        group: "problem",
    },
    Term {
        key: "false_rejects",
        label: "False rejects",
        weight: -2.0,
        definition: "reject verdicts where the next implementation run changed none of the open findings' cited paths and the next review accepted. A false reject counts here instead of against the implementer.",
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
        weight: -2.0,
        definition: "Regressions with escape review_missed against tasks the row's review accepted, whenever filed, each weighted by the link's severity: critical 8, high 4, medium 2, low 1, nit 0.25, so one costs -16 to -0.5.",
        group: "problem",
    },
    Term {
        key: "incomplete_reviews",
        label: "Incomplete reviews",
        weight: -0.5,
        definition: "incomplete verdicts whose block the run failure classification does not name as infrastructure.",
        group: "problem",
    },
];

/// How one review turned out, as the role panel splits a row's reviews.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// Accepted as implemented, with no `review_missed` regression.
    HeldUp,
    /// A finding or fix that held, or a review job finding CONFIRMED or,
    /// unverified, landed.
    Caught,
    /// A false reject, or a review job none of whose findings held and at
    /// least one of which verification REJECTED.
    FalseAlarm,
    /// Accepted work that a `review_missed` regression came back to.
    Missed,
    /// Not judged yet, incomplete, fixes that failed revalidation, or a
    /// review job whose findings were dropped without a verdict.
    Unresolved,
}

impl Outcome {
    const ALL: [Outcome; 5] = [
        Outcome::HeldUp,
        Outcome::Caught,
        Outcome::FalseAlarm,
        Outcome::Missed,
        Outcome::Unresolved,
    ];

    fn stat(self) -> (&'static str, &'static str) {
        match self {
            Outcome::HeldUp => ("outcome_held_up", "Accepted and held up"),
            Outcome::Caught => ("outcome_caught", "Caught a real problem"),
            Outcome::FalseAlarm => ("outcome_false_alarm", "False alarm"),
            Outcome::Missed => ("outcome_missed", "Missed a regression"),
            Outcome::Unresolved => (
                "outcome_unresolved",
                "Unresolved (not judged yet, incomplete, failed fixes or findings dropped unverified)",
            ),
        }
    }
}

/// One row's reviews in the window while they are read.
#[derive(Default)]
struct Row<'a> {
    units: usize,
    /// Whether any unit has been judged: a gate settlement, a filed finding
    /// with a verdict or a terminal task, or a charge.
    judged: bool,
    provisional: usize,
    without_model: usize,
    review_jobs: usize,
    tallies: BTreeMap<&'static str, Tally<'a>>,
    outcomes: BTreeMap<&'static str, usize>,
    /// Each review gate settlement's tier and whether its verdict passed.
    verdicts: Vec<(&'static str, bool)>,
    seconds: Vec<u64>,
}

impl<'a> Row<'a> {
    fn unit(&mut self, at: DateTime<Utc>, model: Option<&str>, lookback: DateTime<Utc>) {
        self.units += 1;
        if at >= lookback {
            self.provisional += 1;
        }
        if model.is_none() {
            self.without_model += 1;
        }
    }

    fn add(&mut self, key: &'static str, task: &'a TaskFacts, count: f64, occurrence: Occurrence) {
        self.tallies
            .entry(key)
            .or_default()
            .add(task, count, Some(occurrence));
    }

    fn outcome(&mut self, outcome: Outcome) {
        *self.outcomes.entry(outcome.stat().0).or_default() += 1;
    }
}

/// One row per reviewer crew and model with a review settled, or a review
/// job attempt started, since `since`.
fn rows(facts: &AgentFacts, since: DateTime<Utc>, _min_support: usize) -> Vec<RowInput<'_>> {
    let lookback = facts.computed_at - Duration::days(MISS_LOOKBACK_DAYS);
    let by_id: HashMap<&str, &TaskFacts> = facts
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect();
    // Missed regressions by the review they are charged to: its task and
    // settlement time.
    let mut missed: HashMap<(&str, DateTime<Utc>), usize> = HashMap::new();
    for charge in &facts.charges {
        if charge.problem == Problem::Regression
            && charge.owner == Owner::Role(AgentRole::Reviewers)
        {
            *missed
                .entry((charge.task_id.as_str(), charge.anchor))
                .or_default() += 1;
        }
    }

    let mut rows: BTreeMap<(String, String), Row<'_>> = BTreeMap::new();
    for task in &facts.tasks {
        for review in task.reviews.iter().filter(|review| review.at >= since) {
            let runner = &review.reviewer;
            let row = rows
                .entry(row_key(runner.crew.as_deref(), runner.model.as_deref()))
                .or_default();
            row.unit(review.at, runner.model.as_deref(), lookback);
            row.judged = true;
            row.verdicts
                .push((tier(task.complexity), review.verdict.passed()));
            row.seconds.extend(review.seconds);
            let run_id = task
                .attempt_at(review.at)
                .and_then(|index| task.attempts[index].runner.run_id.clone());
            let occurrence = |detail: String| Occurrence {
                at: review.at,
                detail: format!("review attempt {}: {detail}", review.attempt_id),
                run_id: run_id.clone(),
            };
            let held = held_findings(review);
            for finding in &held {
                row.add(
                    "findings_held",
                    task,
                    severity_weight(&finding.severity),
                    occurrence(finding.line.clone()),
                );
            }
            let missed = missed.contains_key(&(task.id.as_str(), review.at));
            let fixes_held =
                review.verdict == ReviewVerdict::AcceptWithFixes && !review.fix_failed && !missed;
            if fixes_held {
                row.add(
                    "fixes_held",
                    task,
                    1.0,
                    occurrence("fixes passed owner revalidation".to_string()),
                );
            }
            row.outcome(if review.verdict.passed() && missed {
                Outcome::Missed
            } else if review.false_reject() {
                Outcome::FalseAlarm
            } else if !held.is_empty() || fixes_held {
                Outcome::Caught
            } else if review.verdict == ReviewVerdict::Accept {
                Outcome::HeldUp
            } else {
                Outcome::Unresolved
            });
        }
    }

    // Review jobs count through their filed findings: one unit per attempt
    // that filed any.
    let mut jobs: BTreeMap<(&str, usize), Vec<&TaskFacts>> = BTreeMap::new();
    for task in facts.tasks.iter().filter(|task| task.filed.is_some()) {
        if let Some((filer, index)) = filing_attempt(&by_id, task) {
            jobs.entry((filer.id.as_str(), index))
                .or_default()
                .push(task);
        }
    }
    for ((filer, index), findings) in jobs {
        let attempt = &by_id[filer].attempts[index];
        if attempt.start < since {
            continue;
        }
        let runner = &attempt.runner;
        let row = rows
            .entry(row_key(runner.crew.as_deref(), runner.model.as_deref()))
            .or_default();
        row.unit(attempt.start, runner.model.as_deref(), lookback);
        row.review_jobs += 1;
        let (mut held, mut rejected) = (0, 0);
        for task in findings {
            let Some(filed) = task.filed.as_ref() else {
                continue;
            };
            // A verdict wins; without one, the task's outcome judges it.
            let (weight, at, detail) = match filed.verdict {
                Some((FindingVerdict::Confirmed, at, severity)) => (
                    severity_or_low(severity),
                    at,
                    format!("finding verification CONFIRMED a finding {filer} filed"),
                ),
                Some((FindingVerdict::Rejected, _, _)) => {
                    // Charged as a rejected finding by attribution.
                    row.judged = true;
                    rejected += 1;
                    continue;
                }
                None if task.landed => (
                    severity_or_low(filed.severity),
                    task.landed_at.unwrap_or(task.created_at),
                    format!(
                        "a finding {filer} filed landed without a verification verdict, at {} severity",
                        filed.severity.unwrap_or(DefectSeverity::Low).as_str()
                    ),
                ),
                None => {
                    // Dropped is judged but neutral; open is not judged yet.
                    row.judged |= task.dropped;
                    continue;
                }
            };
            row.judged = true;
            held += 1;
            row.add(
                "findings_held",
                task,
                weight,
                Occurrence {
                    at,
                    detail,
                    run_id: runner.run_id.clone(),
                },
            );
        }
        row.outcome(match (held, rejected) {
            (1.., _) => Outcome::Caught,
            (0, 1..) => Outcome::FalseAlarm,
            _ => Outcome::Unresolved,
        });
    }

    // Mock comparison reviews count under purpose `visual_review`
    // (§11.4): one unit per recorded review, judged by its in-line
    // verification verdicts.
    for task in &facts.tasks {
        let Some(visual) = &task.visual_review else {
            continue;
        };
        if visual.at < since {
            continue;
        }
        let row = rows
            .entry(row_key(visual.reviewer_crew.as_deref(), None))
            .or_default();
        row.unit(visual.at, None, lookback);
        let (mut held, mut rejected) = (0, 0);
        for (verdict, severity, title) in &visual.findings {
            row.judged = true;
            let occurrence = Occurrence {
                at: visual.at,
                detail: format!("visual review (purpose visual_review): {title}"),
                run_id: None,
            };
            match verdict {
                FindingVerdict::Confirmed => {
                    held += 1;
                    row.add("findings_held", task, severity_weight(severity), occurrence);
                }
                FindingVerdict::Rejected => {
                    rejected += 1;
                    row.add(
                        "rejected_findings",
                        task,
                        severity_weight(severity),
                        occurrence,
                    );
                }
            }
        }
        row.outcome(match (held, rejected) {
            (1.., _) => Outcome::Caught,
            (0, 1..) => Outcome::FalseAlarm,
            _ => Outcome::Unresolved,
        });
    }

    for charge in facts.charges.iter().filter(|charge| {
        charge.owner == Owner::Role(AgentRole::Reviewers) && charge.anchor >= since
    }) {
        let key = match charge.problem {
            Problem::DisposedFinding | Problem::RejectedFinding => "rejected_findings",
            Problem::FalseReject => "false_rejects",
            Problem::FailedFix => "failed_fixes",
            Problem::Regression => "missed_regressions",
            Problem::IncompleteReview => "incomplete_reviews",
            _ => continue,
        };
        let Some(task) = by_id.get(charge.task_id.as_str()) else {
            continue;
        };
        let row = rows
            .entry(row_key(charge.crew.as_deref(), charge.model.as_deref()))
            .or_default();
        row.judged = true;
        row.add(
            key,
            task,
            charge.severity_weight(),
            Occurrence {
                at: charge.at,
                detail: format!("{}: {}", charge.problem.as_str(), charge.detail),
                run_id: charge.run_id.clone(),
            },
        );
    }

    let peers = PeerRates::new(&rows);
    let comparisons: Vec<_> = rows
        .iter()
        .map(|(key, row)| peers.compare(key, &row.verdicts))
        .collect();
    rows.into_iter()
        .zip(comparisons)
        .map(|((key, row), peers)| {
            let stats = stats(&row, peers);
            RowInput {
                denominators: BTreeMap::new(),
                crew: key.0,
                model: key.1,
                units: row.units,
                runs_without_model: row.without_model,
                provisional: Some(row.provisional),
                tallies: row.tallies,
                expected: None,
                reviewed_units: None,
                unjudged: !row.judged,
                stats,
            }
        })
        .collect()
}

/// The findings of `review` that held: fixes that passed owner revalidation,
/// and open findings on a `reject` whose next run changed a path they cite.
fn held_findings(review: &Review) -> Vec<&Finding> {
    match (review.verdict, &review.outcome) {
        (ReviewVerdict::AcceptWithFixes, _) if !review.fix_failed => review
            .findings
            .iter()
            .filter(|finding| finding.state == FindingState::Fixed)
            .collect(),
        (ReviewVerdict::Reject, RejectOutcome::Judged { changed, .. }) => review
            .open()
            .filter(|finding| finding.paths.iter().any(|path| changed.contains(path)))
            .collect(),
        _ => Vec::new(),
    }
}

/// The weight of a review gate finding's severity; a value off the scale
/// weighs 1.
fn severity_weight(severity: &str) -> f64 {
    DefectSeverity::parse(severity).map_or(1.0, DefectSeverity::weight)
}

/// The weight of a filed finding that held at its recorded severity: the one
/// verification rated, else the one rated on its own links; without one it
/// weighs as `low`.
fn severity_or_low(severity: Option<DefectSeverity>) -> f64 {
    severity.unwrap_or(DefectSeverity::Low).weight()
}

fn tier(complexity: Option<TaskComplexity>) -> &'static str {
    complexity.map_or("unassessed", TaskComplexity::as_str)
}

/// Each row's passed and total review gate settlements on one tier.
type TierCounts<'k> = BTreeMap<&'k (String, String), (usize, usize)>;

/// Every row's review gate verdicts by tier, to compare a row's accept rate
/// with the other reviewers' on the same tiers.
struct PeerRates<'k> {
    tiers: BTreeMap<&'static str, TierCounts<'k>>,
}

/// A row's accept rate against its peers': the rate the other reviewers
/// would have had on the row's own tier mix, over the reviews whose tier any
/// peer reviewed.
struct PeerComparison {
    expected: f64,
    observed: f64,
}

impl<'k> PeerRates<'k> {
    fn new(rows: &'k BTreeMap<(String, String), Row<'_>>) -> Self {
        let mut tiers: BTreeMap<&'static str, TierCounts<'k>> = BTreeMap::new();
        for (key, row) in rows {
            for (tier, passed) in &row.verdicts {
                let entry = tiers.entry(tier).or_default().entry(key).or_default();
                entry.0 += usize::from(*passed);
                entry.1 += 1;
            }
        }
        Self { tiers }
    }

    fn compare(
        &self,
        key: &(String, String),
        verdicts: &[(&'static str, bool)],
    ) -> Option<PeerComparison> {
        let (mut compared, mut passed, mut expected) = (0.0, 0.0, 0.0);
        for (tier, accepted) in verdicts {
            let (peer_passed, peer_total) = self
                .tiers
                .get(tier)
                .into_iter()
                .flatten()
                .filter(|(other, _)| **other != key)
                .fold((0, 0), |(p, t), (_, (passed, total))| {
                    (p + passed, t + total)
                });
            if peer_total == 0 {
                continue;
            }
            compared += 1.0;
            passed += f64::from(u8::from(*accepted));
            expected += peer_passed as f64 / peer_total as f64;
        }
        (compared > 0.0).then(|| PeerComparison {
            expected: expected / compared,
            observed: passed / compared,
        })
    }
}

/// The row's shown-not-scored context: its review counts, accept rate
/// against peers, verdict outcomes and median review time.
fn stats(row: &Row<'_>, peers: Option<PeerComparison>) -> Vec<Stat> {
    let gate = row.verdicts.len();
    let accepted = row.verdicts.iter().filter(|(_, passed)| *passed).count();
    let percent = |rate: f64| number(rate * 100.0);
    let mut stats = vec![
        stat("gate_reviews", "Review gate settlements", json!(gate)),
        stat(
            "review_jobs",
            "Review job runs that filed findings",
            json!(row.review_jobs),
        ),
        stat(
            "accept_rate",
            "Accept rate (%)",
            if gate > 0 {
                percent(accepted as f64 / gate as f64)
            } else {
                Value::Null
            },
        ),
        stat(
            "peer_accept_rate",
            "Peers' accept rate on the same tiers (%)",
            peers
                .as_ref()
                .map_or(Value::Null, |peers| percent(peers.expected)),
        ),
        stat(
            "accepts_often",
            "Accepts often",
            json!(peers.as_ref().is_some_and(|peers| {
                (peers.observed - peers.expected) * 100.0 >= ACCEPTS_OFTEN_POINTS
            })),
        ),
    ];
    for outcome in Outcome::ALL {
        let (key, label) = outcome.stat();
        stats.push(stat(
            key,
            label,
            json!(row.outcomes.get(key).copied().unwrap_or(0)),
        ));
    }
    stats.push(stat(
        "median_review_minutes",
        "Median review minutes",
        median(&row.seconds).map_or(Value::Null, |seconds| number(seconds / 60.0)),
    ));
    stats
}

fn median(values: &[u64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        len if len % 2 == 1 => Some(sorted[middle] as f64),
        _ => Some((sorted[middle - 1] + sorted[middle]) as f64 / 2.0),
    }
}

fn stat(key: &'static str, label: &'static str, value: Value) -> Stat {
    Stat { key, label, value }
}
