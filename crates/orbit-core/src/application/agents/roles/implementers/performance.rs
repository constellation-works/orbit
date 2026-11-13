use std::collections::{BTreeMap, BTreeSet};

use orbit_config::{TaskTypeDefinition, classify_task_type};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use crate::application::agents::attribution::{Charge, Credit, Owner};
use crate::application::agents::effort::{
    EffortSource, MainActivity, MainInvocationUnit, ModelEffortKey, RecordedEffort,
    main_effort_with_source, model_effort_key,
};
use crate::application::agents::facts::{Attempt, ExecutionMode, Problem, Review, TaskFacts};
use crate::application::agents::score::{
    self, Denominator, Occurrence, REVIEW_MIN_SUPPORT, RowInput, Stat, Tally, Term, number,
    sample_units,
};
use crate::application::agents::{AgentFacts, AgentRole, ExecutionScope, WindowBounds};

pub(super) const TERMS: &[Term] = &[
    Term {
        key: "weighted_landed",
        label: "Landed",
        weight: 1.0,
        definition: "Landed tasks whose final implementation run is the row's, one point each, whatever their complexity. Tasks tagged no-diff-expected when an attempt was admitted are not implementer work and are left out of every implementer count.",
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
        key: ESCAPED_REGRESSIONS,
        label: "Escaped regressions",
        weight: -0.5,
        definition: "regression_from links with escape no_test against the row's landed tasks, mostly delivery-review findings, each at its severity weight (critical 8, high 4, medium 2, low 1, nit 0.25), so a medium one costs as much as a review reject. Expected counts use other rows' rate for the same tier and expected change size, falling back to other rows in the tier, then the fleet, or zero without peers; never the row's own escapes or diff. A link whose defect site a pre-landing review finding on the row's candidate already cited or fixed is that review's reject or repair and is not counted again.",
        group: "problem",
    },
];

/// Terms whose counts and expectations are rated per reviewed delivery.
pub(super) const REVIEW_TERM_UNITS: &[(&str, &str)] = &[
    ("first_pass_accepts", "reviewed deliveries"),
    ("review_rejects", "reviewed deliveries"),
    ("reviewer_repairs", "reviewed deliveries"),
];

pub(super) fn is_implementer_review_term(key: &str) -> bool {
    REVIEW_TERM_UNITS.iter().any(|(term, _)| *term == key)
}

pub(super) type Key = ModelEffortKey;

/// An attempt's row: its model and the recorded effort of its run's main
/// implementing invocation for `task`.
pub(super) fn key(facts: &AgentFacts, task: &TaskFacts, attempt: &Attempt) -> Key {
    key_with_source(facts, task, attempt).0
}

/// [`key`] and the source that decided the attempt's effort.
fn key_with_source(facts: &AgentFacts, task: &TaskFacts, attempt: &Attempt) -> (Key, EffortSource) {
    implementer_key_with_source(
        facts,
        attempt.runner.run_id.as_deref(),
        &task.id,
        attempt.runner.model.as_deref(),
        attempt.runner.reported_effort.as_ref(),
        attempt.runner.crew.as_deref(),
    )
}

/// The shared implementer effort key and its source, using the attempt,
/// charge, or credit's own recorded identity and fallback data.
fn implementer_key_with_source(
    facts: &AgentFacts,
    run_id: Option<&str>,
    task_id: &str,
    model: Option<&str>,
    reported: Option<&RecordedEffort>,
    crew: Option<&str>,
) -> (Key, EffortSource) {
    let (effort, source) = main_effort_with_source(
        facts,
        MainInvocationUnit {
            run_id,
            activity: MainActivity::Implement,
            task_id: Some(task_id),
            model,
            activity_id: None,
            after: None,
            until: None,
            reported,
            crew,
        },
    );
    (model_effort_key(model, effort), source)
}

/// A charge's row, from the run it charges.
fn charge_key(facts: &AgentFacts, charge: &Charge) -> Key {
    implementer_key_with_source(
        facts,
        charge.run_id.as_deref(),
        &charge.task_id,
        charge.model.as_deref(),
        charge.reported_effort.as_ref(),
        charge.crew.as_deref(),
    )
    .0
}

/// A credit's row, from the run it credits.
fn credit_key(facts: &AgentFacts, credit: &Credit) -> Key {
    implementer_key_with_source(
        facts,
        credit.run_id.as_deref(),
        &credit.task_id,
        credit.model.as_deref(),
        credit.reported_effort.as_ref(),
        credit.crew.as_deref(),
    )
    .0
}

pub(super) fn tier(task: &TaskFacts) -> &'static str {
    task.complexity.map_or("unassessed", |tier| tier.as_str())
}

/// Whether an attempt counts as implementation in the window: it started
/// in the window, the task was not `no-diff-expected` work when the
/// attempt was admitted, and its run launched an implementer (or is not
/// proven not to have). The one filter the headline rows, the fleet
/// expectations and the by-type cells all share.
pub(super) fn counted(attempt: &Attempt, window: WindowBounds) -> bool {
    window.admits(attempt.start) && attempt.implements()
}

/// Each row's counted tasks whose execution `execution` admits: one task
/// per row, however many counted attempts that row started in the window.
pub(super) fn work_by_row(
    facts: &AgentFacts,
    window: WindowBounds,
    execution: ExecutionScope,
) -> BTreeMap<Key, Vec<&TaskFacts>> {
    let mut work: BTreeMap<Key, Vec<&TaskFacts>> = BTreeMap::new();
    for task in facts
        .tasks
        .iter()
        .filter(|task| execution.admits(task.execution))
    {
        let keys: BTreeSet<_> = task
            .attempts
            .iter()
            .filter(|a| counted(a, window))
            .map(|attempt| key(facts, task, attempt))
            .collect();
        for row in keys {
            work.entry(row).or_default().push(task);
        }
    }
    work
}

pub(super) fn implementer_rows(
    facts: &AgentFacts,
    window: WindowBounds,
    _min_support: usize,
    execution: ExecutionScope,
) -> Vec<RowInput<'_>> {
    evaluate_implementers(facts, window, execution).rows
}

/// One implemented task's expected count per scored term, as its row's
/// expectation sums them: tier rates for every term but escaped
/// regressions, which take the size-adjusted expectation. A review term's
/// expectation is zero for a task without a review of the row's candidate.
pub(super) struct TaskExpectation<'a> {
    pub(super) task: &'a TaskFacts,
    pub(super) reviewed: bool,
    pub(super) expected: BTreeMap<&'static str, f64>,
}

/// The implementer rows and, per row in the same order, each task's
/// expectations: the scorecard reads the rows, the model ranking the tasks.
pub(super) struct Evaluated<'a> {
    pub(super) rows: Vec<RowInput<'a>>,
    pub(super) tasks: Vec<Vec<TaskExpectation<'a>>>,
}

pub(super) fn evaluate_implementers(
    facts: &AgentFacts,
    window: WindowBounds,
    execution: ExecutionScope,
) -> Evaluated<'_> {
    let work = work_by_row(facts, window, execution);
    let mut charges: BTreeMap<&str, Vec<&Charge>> = BTreeMap::new();
    let mut credits: BTreeMap<&str, Vec<&Credit>> = BTreeMap::new();
    for charge in &facts.charges {
        charges.entry(&charge.task_id).or_default().push(charge);
    }
    for credit in &facts.credits {
        credits.entry(&credit.task_id).or_default().push(credit);
    }
    let mut rows: Vec<_> = work
        .iter()
        .map(|(row, tasks)| row_input(facts, window, row.clone(), tasks, &charges, &credits))
        .collect();
    let expected_regressions = super::context::append_diff_context(facts, window, &work, &mut rows);
    let mut fleet: BTreeMap<&str, Fleet> = BTreeMap::new();
    for (row, tasks) in &work {
        for task in tasks {
            let stats = fleet.entry(tier(task)).or_default();
            stats.units += 1;
            stats.reviewed_units +=
                usize::from(first_implementer_review(facts, task, row, window).is_some());
        }
    }
    for row in &rows {
        for (term, tally) in &row.tallies {
            for (task, count) in tally.task_counts() {
                *fleet
                    .entry(tier(task))
                    .or_default()
                    .counts
                    .entry(term)
                    .or_default() += count;
            }
        }
    }
    let references = super::tier_reference::tier_references(&fleet);
    let mut per_task = Vec::with_capacity(rows.len());
    for ((row, (key, tasks)), regressions) in rows.iter_mut().zip(&work).zip(&expected_regressions)
    {
        let tiers: BTreeSet<_> = tasks.iter().map(|task| tier(task)).collect();
        row.stats.push(Stat {
            key: "tier_adjustments",
            label: "Sparse tier adjustments",
            value: json!(
                tiers
                    .iter()
                    .filter_map(|tier| references.adjustments.get(tier))
                    .collect::<Vec<_>>()
            ),
        });
        let expectations: Vec<TaskExpectation<'_>> = tasks
            .iter()
            .zip(regressions)
            .map(|(&task, &regressions)| {
                let reviewed = first_implementer_review(facts, task, key, window).is_some();
                let expected = TERMS
                    .iter()
                    .map(|term| {
                        // Regression risk follows change size, so escaped
                        // regressions take the size-adjusted expectation.
                        if term.key == ESCAPED_REGRESSIONS {
                            return (term.key, regressions);
                        }
                        if is_implementer_review_term(term.key) && !reviewed {
                            return (term.key, 0.0);
                        }
                        (term.key, references.rates[tier(task)][term.key])
                    })
                    .collect();
                TaskExpectation {
                    task,
                    reviewed,
                    expected,
                }
            })
            .collect();
        row.expected = Some(
            TERMS
                .iter()
                .map(|term| {
                    let review_term = is_implementer_review_term(term.key);
                    let expected = expectations
                        .iter()
                        .filter(|task| !review_term || task.reviewed)
                        .map(|task| task.expected[term.key])
                        .sum();
                    (term.key, expected)
                })
                .collect(),
        );
        row.sample_units = sample_units(
            tasks.iter().map(|task| task.id.clone()),
            TERMS,
            &row.tallies,
        );
        let expectations_by_task: BTreeMap<&str, &TaskExpectation<'_>> = expectations
            .iter()
            .map(|task| (task.task.id.as_str(), task))
            .collect();
        for unit in &mut row.sample_units {
            if let Some(task) = expectations_by_task.get(unit.id.as_str()) {
                unit.expected.clone_from(&task.expected);
                if task.reviewed {
                    unit.denominator_terms
                        .extend(REVIEW_TERM_UNITS.iter().map(|(term, _)| (*term, 1)));
                }
            }
        }
        per_task.push(expectations);
    }
    Evaluated {
        rows,
        tasks: per_task,
    }
}

/// The implementer header's `execution_split`: per row (crew and model) with
/// a counted attempt in the window, whatever the selection's scope, its
/// tasks by derived execution mode, and the interactive tasks' outcomes
/// (landed, and regressed by any `regression_from` link), so interactive
/// work stays visible while the default counts leave it out.
pub(super) fn execution_header(
    facts: &AgentFacts,
    window: WindowBounds,
) -> serde_json::Map<String, Value> {
    #[derive(Default)]
    struct Split {
        crews: BTreeSet<String>,
        managed: usize,
        interactive: usize,
        unclassified: usize,
        interactive_landed: usize,
        interactive_regressed: usize,
    }
    let regressed: BTreeSet<&str> = facts
        .regressions
        .iter()
        .map(|link| link.culprit.as_str())
        .collect();
    let mut splits: BTreeMap<Key, Split> = BTreeMap::new();
    for task in &facts.tasks {
        let attempts: Vec<_> = task
            .attempts
            .iter()
            .filter(|a| counted(a, window))
            .collect();
        let keys: BTreeSet<_> = attempts
            .iter()
            .map(|attempt| key(facts, task, attempt))
            .collect();
        for row in keys {
            let split = splits.entry(row.clone()).or_default();
            for attempt in &attempts {
                if key(facts, task, attempt) == row
                    && let Some(crew) = &attempt.runner.crew
                {
                    split.crews.insert(crew.clone());
                }
            }
            match task.execution {
                ExecutionMode::Managed => split.managed += 1,
                ExecutionMode::Unclassified => split.unclassified += 1,
                ExecutionMode::Interactive => {
                    split.interactive += 1;
                    split.interactive_landed += usize::from(task.landed);
                    split.interactive_regressed +=
                        usize::from(regressed.contains(task.id.as_str()));
                }
            }
        }
    }
    let rows: Vec<Value> = splits
        .into_iter()
        .map(|((model, effort), split)| {
            json!({
                "crew": split.crews.first(),
                "crews": split.crews,
                "model": model,
                "reasoning_effort": effort,
                "effort_label": crate::application::agents::effort::effort_label(&effort),
                "label": crate::application::agents::effort::row_label(&model, &effort),
                "managed_tasks": split.managed,
                "interactive_tasks": split.interactive,
                "unclassified_tasks": split.unclassified,
                "interactive_landed": split.interactive_landed,
                "interactive_regressed": split.interactive_regressed,
            })
        })
        .collect();
    let mut header = serde_json::Map::new();
    header.insert("execution_split".into(), Value::Array(rows));
    header.insert("review_min_support".into(), json!(REVIEW_MIN_SUPPORT));
    header
}

/// A later accept cannot erase an earlier reject of this row's candidate.
pub(super) fn first_implementer_review<'a>(
    facts: &AgentFacts,
    task: &'a TaskFacts,
    row: &Key,
    window: WindowBounds,
) -> Option<&'a Review> {
    task.reviews.iter().find(|review| {
        task.attempt_at(review.at).is_some_and(|index| {
            let attempt = &task.attempts[index];
            counted(attempt, window) && key(facts, task, attempt) == *row
        })
    })
}

/// The location evidence retained by a regression site or review citation.
struct DefectLocation<'a> {
    path: &'a str,
    scope: DefectScope<'a>,
}

enum DefectScope<'a> {
    File,
    Directory,
    Symbol(&'a str),
    Lines(u32, u32),
}

impl<'a> DefectLocation<'a> {
    fn parse(value: &'a str) -> Option<Self> {
        let (path, scope) = if let Some(symbol) = value.strip_prefix("symbol:") {
            let (path, identity) = symbol.split_once('#')?;
            let (name, kind) = identity.rsplit_once(':')?;
            if name.is_empty() || kind.is_empty() {
                return None;
            }
            (path, DefectScope::Symbol(identity))
        } else if let Some(path) = value.strip_prefix("dir:") {
            (path.trim_end_matches('/'), DefectScope::Directory)
        } else {
            let path = selector_path(value)?;
            if let Some((path, lines)) = path.rsplit_once(':') {
                let (start, end) = lines.split_once('-').unwrap_or((lines, lines));
                let (start, end) = (start.parse::<u32>().ok()?, end.parse::<u32>().ok()?);
                if start == 0 || end < start {
                    return None;
                }
                (path, DefectScope::Lines(start, end))
            } else {
                (path, DefectScope::File)
            }
        };
        (!path.is_empty()).then_some(Self { path, scope })
    }

    /// A coarse site intentionally covers its file/directory. Precise sites
    /// require comparable evidence; file equality cannot map a symbol to lines.
    fn covers_citation(&self, cited: &Self) -> bool {
        if matches!(self.scope, DefectScope::Directory) {
            return self.path == cited.path
                || (self.path == "." && !cited.path.starts_with('/'))
                || cited
                    .path
                    .strip_prefix(self.path)
                    .is_some_and(|rest| rest.starts_with('/'));
        }
        self.path == cited.path
            && match (&self.scope, &cited.scope) {
                (DefectScope::File, _) => true,
                (DefectScope::Symbol(site), DefectScope::Symbol(cited)) => site == cited,
                (DefectScope::Lines(start, end), DefectScope::Lines(other_start, other_end)) => {
                    start <= other_end && other_start <= end
                }
                _ => false,
            }
    }
}

/// Whether a pre-landing review already charged the row for the defect a
/// regression link names: a `reject` or `accept_with_fixes` on the row's
/// candidate with a finding that cites or changed one of the link's sites.
/// That defect counts once, as the review's verdict. A link without sites
/// names no defect to match and always counts.
fn charged_by_pre_landing_review(
    facts: &AgentFacts,
    task: &TaskFacts,
    row: &Key,
    window: WindowBounds,
    sites: &[String],
) -> bool {
    let sites: Vec<_> = sites
        .iter()
        .filter_map(|site| DefectLocation::parse(site))
        .collect();
    if sites.is_empty() {
        return false;
    }
    task.reviews
        .iter()
        .filter(|review| match review.verdict {
            ReviewVerdict::Reject => !review.false_reject(),
            ReviewVerdict::AcceptWithFixes => true,
            ReviewVerdict::Accept | ReviewVerdict::Incomplete => false,
        })
        .filter(|review| {
            task.attempt_at(review.at).is_some_and(|index| {
                let attempt = &task.attempts[index];
                counted(attempt, window) && key(facts, task, attempt) == *row
            })
        })
        .flat_map(|review| &review.findings)
        .flat_map(|finding| &finding.paths)
        .filter_map(|cited| DefectLocation::parse(cited))
        .any(|cited| sites.iter().any(|site| site.covers_citation(&cited)))
}

/// The scored escaped-regression term, also read by the diff context.
pub(super) const ESCAPED_REGRESSIONS: &str = "escaped_regressions";

/// Minimum fleet row-task observations before a tier supplies reference evidence.
pub(super) const MIN_TIER_TASKS: usize = 30;

#[derive(Default)]
pub(super) struct Fleet {
    pub(super) units: usize,
    pub(super) reviewed_units: usize,
    pub(super) counts: BTreeMap<&'static str, f64>,
}

/// The scored term tallies of one row (or one cell's task subset).
pub(super) fn compute_tallies<'a>(
    facts: &AgentFacts,
    row: &Key,
    tasks: &[&'a TaskFacts],
    charges: &BTreeMap<&str, Vec<&Charge>>,
    credits: &BTreeMap<&str, Vec<&Credit>>,
    window: WindowBounds,
) -> BTreeMap<&'static str, Tally<'a>> {
    let mut tallies: BTreeMap<&'static str, Tally<'a>> = BTreeMap::new();
    for &task in tasks {
        if let Some(review) = first_implementer_review(facts, task, row, window)
            && review.verdict == ReviewVerdict::Accept
        {
            tallies.entry("first_pass_accepts").or_default().add(
                task,
                &task.id,
                1.0,
                Some(Occurrence {
                    at: review.at,
                    detail: "first review accepted the candidate".into(),
                    run_id: task
                        .attempt_at(review.at)
                        .and_then(|index| task.attempts[index].runner.run_id.clone()),
                }),
            );
        }
        if credits
            .get(task.id.as_str())
            .into_iter()
            .flatten()
            .any(|credit| window.admits(credit.anchor) && credit_key(facts, credit) == *row)
        {
            tallies
                .entry("weighted_landed")
                .or_default()
                .add(task, &task.id, 1.0, None);
        }
        for charge in charges
            .get(task.id.as_str())
            .into_iter()
            .flatten()
            .filter(|charge| {
                window.admits(charge.anchor)
                    && charge.owner == Owner::Role(AgentRole::Implementers)
                    && charge_key(facts, charge) == *row
            })
        {
            let term = match charge.problem {
                Problem::ReviewReject => "review_rejects",
                Problem::ReviewFixes => "reviewer_repairs",
                Problem::Regression
                    if charged_by_pre_landing_review(facts, task, row, window, &charge.sites) =>
                {
                    continue;
                }
                Problem::Regression => ESCAPED_REGRESSIONS,
                problem if problem.is_rework() => "crew_rework",
                _ => continue,
            };
            tallies.entry(term).or_default().add(
                task,
                &task.id,
                charge.severity_weight(),
                Some(Occurrence {
                    at: charge.at,
                    detail: charge.detail.clone(),
                    run_id: charge.run_id.clone(),
                }),
            );
        }
    }
    tallies
}

fn row_input<'a>(
    facts: &AgentFacts,
    window: WindowBounds,
    row: Key,
    tasks: &[&'a TaskFacts],
    charges: &BTreeMap<&str, Vec<&Charge>>,
    credits: &BTreeMap<&str, Vec<&Credit>>,
) -> RowInput<'a> {
    let tallies = compute_tallies(facts, &row, tasks, charges, credits, window);
    // Review coverage determines the review terms' population, including
    // the tier mix of their expectations, rather than credit or penalty.
    let reviewed_units = tasks
        .iter()
        .filter(|task| first_implementer_review(facts, task, &row, window).is_some())
        .count();
    let mut tiers: BTreeMap<&str, usize> = BTreeMap::new();
    let mut durations = Vec::new();
    let mut run_ids = BTreeSet::new();
    let mut missing_runs = false;
    let mut runs_without_model = 0;
    // Cost coverage over the same counted attempts the cost calculation
    // walks, attempt by attempt rather than by distinct run: a run shared
    // by two attempts covers both, and one attempt without a priced run
    // suppresses the complete-cohort average below. Display only; it
    // never enters tallies, denominators or scores. Cost and run time count
    // managed tasks only, whatever the scope: an interactive session
    // records neither, so its tasks would only read as missing runs.
    let mut cost_attempts = 0usize;
    let mut cost_measured_attempts = 0usize;
    let mut cost_unpriced_attempts = 0usize;
    let mut cost_missing_run_attempts = 0usize;
    let mut managed_landed = 0usize;
    for &task in tasks {
        *tiers.entry(tier(task)).or_default() += 1;
        if task.execution != ExecutionMode::Managed {
            continue;
        }
        managed_landed += usize::from(
            tallies
                .get("weighted_landed")
                .is_some_and(|tally| tally.task_counts().any(|(landed, _)| landed.id == task.id)),
        );
        for attempt in task
            .attempts
            .iter()
            .filter(|a| counted(a, window) && key(facts, task, a) == row)
        {
            runs_without_model += usize::from(attempt.runner.model.is_none());
            if let Some(end) = attempt.end {
                durations.push((end - attempt.start).num_milliseconds().max(0) as f64 / 1000.0);
            }
            cost_attempts += 1;
            match attempt.runner.run_id.as_deref() {
                Some(id) => {
                    run_ids.insert(id);
                    if facts.run_costs.get(id).copied().flatten().is_some() {
                        cost_measured_attempts += 1;
                    } else {
                        cost_unpriced_attempts += 1;
                    }
                }
                None => {
                    missing_runs = true;
                    cost_missing_run_attempts += 1;
                }
            }
        }
    }
    let cost = if missing_runs || run_ids.is_empty() {
        None
    } else {
        run_ids.iter().try_fold(0.0, |total, id| {
            facts
                .run_costs
                .get(*id)
                .copied()
                .flatten()
                .map(|cost| total + cost)
        })
    };
    let median = score::median(&mut durations);
    RowInput {
        escaped_regressions_fallback: None,
        denominators: REVIEW_TERM_UNITS
            .iter()
            .map(|(key, _)| {
                (
                    *key,
                    Denominator {
                        units: reviewed_units,
                        // Zero reviews is unmeasured, not a provisional estimate or
                        // a measured zero. Fewer than the minimum support is
                        // provisional: rated and shown, left out of the raw score.
                        min_support: if reviewed_units > 0 {
                            REVIEW_MIN_SUPPORT
                        } else {
                            0
                        },
                    },
                )
            })
            .collect(),
        reviewed_units: Some(reviewed_units),
        unjudged: false,
        sample_units: Vec::new(),
        exposure_units: Vec::new(),
        crew: crews_of(facts, window, &row, tasks)
            .first()
            .cloned()
            .unwrap_or_else(|| crate::application::agents::CREW_UNKNOWN.to_string()),
        crews: crews_of(facts, window, &row, tasks),
        effort_sources: effort_sources_of(facts, window, &row, tasks),
        model: row.0,
        effort: row.1,
        units: tasks.len(),
        runs_without_model,
        provisional: None,
        tallies,
        expected: None,
        stats: vec![
            stat(
                "cost_per_weighted_landed",
                "Cost per landed task (USD)",
                cost.filter(|_| managed_landed > 0)
                    .map(|cost| json!(cost / managed_landed as f64))
                    .unwrap_or(Value::Null),
            ),
            stat(
                "cost_coverage",
                "Cost coverage (counted attempts)",
                json!(format!(
                    "{cost_measured_attempts} measured · {cost_unpriced_attempts} unpriced · {cost_missing_run_attempts} missing run of {cost_attempts} counted attempts"
                )),
            ),
            stat(
                "cost_coverage_attempts",
                "Cost coverage denominator (counted attempts)",
                json!(cost_attempts),
            ),
            stat(
                "cost_measured_attempts",
                "Cost measured attempts",
                json!(cost_measured_attempts),
            ),
            stat(
                "cost_unpriced_attempts",
                "Cost unpriced attempts",
                json!(cost_unpriced_attempts),
            ),
            stat(
                "cost_missing_run_attempts",
                "Cost missing-run attempts",
                json!(cost_missing_run_attempts),
            ),
            stat(
                "median_run_seconds",
                "Median completed run (seconds)",
                median.map(number).unwrap_or(Value::Null),
            ),
            stat(
                "tasks_by_tier",
                "Tasks by tier",
                json!(
                    tiers
                        .iter()
                        .map(|(tier, counts)| format!("{tier}: {counts}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
        ],
    }
}

/// The crews whose counted attempts on `tasks` fall in `row`, sorted.
pub(super) fn crews_of(
    facts: &AgentFacts,
    window: WindowBounds,
    row: &Key,
    tasks: &[&TaskFacts],
) -> Vec<String> {
    let mut crews = BTreeSet::new();
    for task in tasks {
        for attempt in task
            .attempts
            .iter()
            .filter(|attempt| counted(attempt, window) && &key(facts, task, attempt) == row)
        {
            if let Some(crew) = &attempt.runner.crew {
                crews.insert(crew.clone());
            }
        }
    }
    crews.into_iter().collect()
}

/// The dispatch identity the crew-label fallback gives each implementation
/// run whose attempt has no recorded effort but whose recorded crew label is
/// a `<crew>@<effort>` reference, by run id [ORB-24150]. The model ranking's
/// effort split counts that run's identity-less invocations under it, so the
/// split and the scorecard rows agree.
pub(in crate::application::agents) fn crew_label_run_identities(
    facts: &AgentFacts,
) -> std::collections::HashMap<String, orbit_types::telemetry::AgentExecutionIdentity> {
    let mut identities = std::collections::HashMap::new();
    for task in &facts.tasks {
        for attempt in &task.attempts {
            let (Some(run_id), Some(crew)) = (&attempt.runner.run_id, &attempt.runner.crew) else {
                continue;
            };
            let ((_, effort), source) = key_with_source(facts, task, attempt);
            if source != EffortSource::CrewLabel {
                continue;
            }
            let Some(effort) = effort.and_then(|effort| effort.parse().ok()) else {
                continue;
            };
            identities.insert(
                run_id.clone(),
                orbit_types::telemetry::AgentExecutionIdentity {
                    crew: Some(crew.clone()),
                    reasoning_effort: Some(effort),
                },
            );
        }
    }
    identities
}

/// The row's tasks by the source that decided the effort of their latest
/// counted attempt in the row.
fn effort_sources_of(
    facts: &AgentFacts,
    window: WindowBounds,
    row: &Key,
    tasks: &[&TaskFacts],
) -> BTreeMap<&'static str, usize> {
    let mut sources = BTreeMap::new();
    for task in tasks {
        let source = task
            .attempts
            .iter()
            .rev()
            .filter(|attempt| counted(attempt, window))
            .map(|attempt| key_with_source(facts, task, attempt))
            .find(|(key, _)| key == row)
            .map(|(_, source)| source);
        if let Some(source) = source {
            *sources.entry(source.as_str()).or_default() += 1;
        }
    }
    sources
}

pub(super) fn stat(key: &'static str, label: &'static str, value: Value) -> Stat {
    Stat { key, label, value }
}

/// The tags in force when a task's counted work was admitted (its earliest
/// counted attempt in the window), so a tag added after an attempt started
/// cannot reclassify work already scored.
pub(super) fn admitted_tags(task: &TaskFacts, window: WindowBounds) -> &[String] {
    task.attempts
        .iter()
        .filter(|a| counted(a, window))
        .min_by_key(|a| a.start)
        .map_or(&task.tags, |a| &a.tags)
}

/// The currently configured type of a task, classified as the by-type
/// cells classify it.
pub(super) fn configured_type(
    task: &TaskFacts,
    window: WindowBounds,
    types: &[TaskTypeDefinition],
) -> String {
    classify_task_type(types, admitted_tags(task, window), &task.context_files).to_string()
}

/// The path a context-file selector names, without its prefix. `module:`
/// and `command:` selectors name no path.
pub(super) fn selector_path(selector: &str) -> Option<&str> {
    if let Some(path) = selector.strip_prefix("file:") {
        Some(path)
    } else if let Some(path) = selector.strip_prefix("dir:") {
        Some(path)
    } else if let Some(remainder) = selector.strip_prefix("symbol:") {
        let (path, _) = remainder.split_once('#')?;
        if path.is_empty() { None } else { Some(path) }
    } else if selector.starts_with("module:") || selector.starts_with("command:") {
        None
    } else {
        Some(selector)
    }
}

/// The top-level path prefixes of a task's context-file selectors: the
/// first path component of each named path, deduplicated and sorted.
pub(super) fn path_prefixes(task: &TaskFacts) -> Vec<String> {
    let mut prefixes = BTreeSet::new();
    for selector in &task.context_files {
        if let Some(path) = selector_path(selector)
            && let Some(first) = path.split('/').find(|part| !part.is_empty())
        {
            prefixes.insert(first.to_string());
        }
    }
    prefixes.into_iter().collect()
}
