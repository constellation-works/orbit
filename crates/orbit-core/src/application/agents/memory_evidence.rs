//! The work a crew memory dreaming run reflects on (crew memory §6.2): for
//! one model, the problem records behind its implementer, reviewer and pilot
//! rows, pooled over that model's crews, and nothing about how they are
//! measured. Each term becomes a fixed plain description; counts, rates,
//! weights, contributions, expected counts, units, ranks and every other
//! model's rows stay out. Rendering, token stripping and the size cap are
//! the crew memory routine's.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;

use super::facts::TaskFacts;
use super::score::{Occurrence, RowInput};
use super::{AgentFacts, AgentQuery, AgentRole, DEFAULT_MIN_SUPPORT, ExecutionScope, roles};

/// The implementer and pilot window: the trailing 7 days.
const RECENT_WINDOW: &str = "7d";
/// The settled reviewer window: 30 days ending 30 days ago, so a review's
/// misses have had time to surface.
const REVIEWER_WINDOW: &str = "30d";
const REVIEWER_UNTIL_DAYS: i64 = 30;

/// The problem terms each role's section carries, with the plain
/// description that replaces the term's label. Implementers carry every
/// problem term; reviewers their missed regressions and overturned reviews;
/// pilots their misjudged footprints.
const IMPLEMENTER_PROBLEMS: &[(&str, &str)] = &[
    (
        "review_rejects",
        "a review sent this change back to be redone",
    ),
    (
        "reviewer_repairs",
        "a review accepted this change only after repairing it",
    ),
    (
        "crew_rework",
        "this change had to be run again, was blocked or did not merge, because of how it was implemented",
    ),
    (
        "escaped_regressions",
        "a regression was later traced to this change",
    ),
];
const REVIEWER_PROBLEMS: &[(&str, &str)] = &[
    (
        "missed_regressions",
        "a change this model's review accepted later had a regression traced to it",
    ),
    (
        "false_rejects",
        "this model's review sent a change back, and the next review accepted it without the cited paths changing",
    ),
    (
        "rejected_findings",
        "a finding this model raised in review was later decided to be wrong",
    ),
];
const PILOT_PROBLEMS: &[(&str, &str)] = &[
    (
        "missed_scope_blocks",
        "this model's assessment found no problem, and the task then blocked on a scope decision",
    ),
    (
        "costly_misses",
        "the task changed files this model's predicted footprint did not cover, and that caused a conflict or a refused lock",
    ),
    (
        "lock_wait_hours",
        "this model's predicted footprint reserved paths the task never changed, and other tasks waited on them",
    ),
];

/// One record behind a task's problem.
#[derive(Debug, Clone)]
pub(crate) struct EvidenceOccurrence {
    pub(crate) at: DateTime<Utc>,
    pub(crate) detail: String,
}

/// A regression fix filed against a task (`regression_from`).
#[derive(Debug, Clone)]
pub(crate) struct EvidenceRegressionFix {
    pub(crate) task_id: String,
    pub(crate) title: String,
}

/// One task behind a problem, with what happened on it.
#[derive(Debug, Clone)]
pub(crate) struct EvidenceTask {
    pub(crate) task_id: String,
    pub(crate) title: String,
    /// Newest first.
    pub(crate) occurrences: Vec<EvidenceOccurrence>,
    pub(crate) regression_fixes: Vec<EvidenceRegressionFix>,
    /// The task's path selectors, which delivery widens over every path a
    /// landed change touched.
    pub(crate) paths: Vec<String>,
}

impl EvidenceTask {
    /// The newest record, or the epoch for a task with none.
    pub(crate) fn latest_at(&self) -> DateTime<Utc> {
        self.occurrences
            .iter()
            .map(|occurrence| occurrence.at)
            .max()
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
    }
}

/// One problem with its plain description and the tasks behind it.
#[derive(Debug, Clone)]
pub(crate) struct EvidenceProblem {
    pub(crate) description: &'static str,
    /// Newest occurrence first.
    pub(crate) tasks: Vec<EvidenceTask>,
}

/// One role's problems in its window.
#[derive(Debug, Clone)]
pub(crate) struct EvidenceSection {
    pub(crate) since: String,
    pub(crate) until: String,
    pub(crate) problems: Vec<EvidenceProblem>,
}

/// One model's evidence pack, before rendering.
#[derive(Debug, Clone)]
pub(crate) struct ModelEvidence {
    pub(crate) implementer: EvidenceSection,
    pub(crate) reviewer: EvidenceSection,
    pub(crate) pilot: EvidenceSection,
}

/// Each model's implementer units in the trailing 7 days, summed over its
/// crews. Rows with no recorded model are left out: they get no memory.
pub(crate) fn implementer_units_by_model(
    facts: &AgentFacts,
) -> Result<BTreeMap<String, usize>, OrbitError> {
    let query = recent_query()?;
    let mut units = BTreeMap::new();
    for row in role_rows(facts, AgentRole::Implementers, &query) {
        if row.model != super::MODEL_UNKNOWN {
            *units.entry(row.model.clone()).or_default() += row.units;
        }
    }
    Ok(units)
}

/// `model`'s evidence: its implementer and pilot problems in the trailing 7
/// days and its reviewer problems in the settled 30 days ending 30 days ago.
pub(crate) fn model_evidence(facts: &AgentFacts, model: &str) -> Result<ModelEvidence, OrbitError> {
    let recent = recent_query()?;
    let until = (Utc::now() - Duration::days(REVIEWER_UNTIL_DAYS))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let settled = AgentQuery::parse(Some(REVIEWER_WINDOW), Some(DEFAULT_MIN_SUPPORT))?
        .with_until(Some(&until))?;
    Ok(ModelEvidence {
        implementer: section(
            facts,
            AgentRole::Implementers,
            &recent,
            model,
            IMPLEMENTER_PROBLEMS,
        ),
        reviewer: section(
            facts,
            AgentRole::Reviewers,
            &settled,
            model,
            REVIEWER_PROBLEMS,
        ),
        pilot: section(facts, AgentRole::Pilots, &recent, model, PILOT_PROBLEMS),
    })
}

fn recent_query() -> Result<AgentQuery, OrbitError> {
    AgentQuery::parse(Some(RECENT_WINDOW), Some(DEFAULT_MIN_SUPPORT))
}

fn role_rows<'a>(facts: &'a AgentFacts, role: AgentRole, query: &AgentQuery) -> Vec<RowInput<'a>> {
    let spec = roles::spec(role);
    let window = query.bounds(facts);
    spec.rows
        .map(|rows| rows(facts, window, query.min_support, ExecutionScope::Managed))
        .unwrap_or_default()
}

fn section(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    model: &str,
    problems: &[(&'static str, &'static str)],
) -> EvidenceSection {
    let window = query.bounds(facts);
    let rows: Vec<RowInput<'_>> = role_rows(facts, role, query)
        .into_iter()
        .filter(|row| row.model == model)
        .collect();
    let titles: BTreeMap<&str, &TaskFacts> = facts
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect();
    let mut out = Vec::new();
    for (key, description) in problems {
        // Pool the term's tasks over the model's crews.
        let mut tasks: BTreeMap<&str, (&TaskFacts, Vec<&Occurrence>)> = BTreeMap::new();
        for row in &rows {
            let Some(tally) = row.tallies.get(key) else {
                continue;
            };
            for (task, count, occurrences) in tally.task_records() {
                if count == 0.0 && occurrences.is_empty() {
                    continue;
                }
                tasks
                    .entry(task.id.as_str())
                    .or_insert_with(|| (task, Vec::new()))
                    .1
                    .extend(occurrences);
            }
        }
        if tasks.is_empty() {
            continue;
        }
        let mut entries: Vec<EvidenceTask> = tasks
            .into_values()
            .map(|(task, occurrences)| evidence_task(facts, &titles, task, occurrences))
            .collect();
        entries.sort_by(|left, right| {
            right
                .latest_at()
                .cmp(&left.latest_at())
                .then(left.task_id.cmp(&right.task_id))
        });
        out.push(EvidenceProblem {
            description,
            tasks: entries,
        });
    }
    EvidenceSection {
        since: window.since().to_rfc3339_opts(SecondsFormat::Secs, true),
        until: window
            .reported_until()
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        problems: out,
    }
}

fn evidence_task(
    facts: &AgentFacts,
    titles: &BTreeMap<&str, &TaskFacts>,
    task: &TaskFacts,
    occurrences: Vec<&Occurrence>,
) -> EvidenceTask {
    let mut seen = BTreeSet::new();
    let mut occurrences: Vec<EvidenceOccurrence> = occurrences
        .into_iter()
        .filter(|occurrence| seen.insert((occurrence.at, occurrence.detail.clone())))
        .map(|occurrence| EvidenceOccurrence {
            at: occurrence.at,
            detail: occurrence.detail.clone(),
        })
        .collect();
    occurrences.sort_by_key(|occurrence| std::cmp::Reverse(occurrence.at));
    let regression_fixes = facts
        .regressions
        .iter()
        .filter(|link| link.culprit == task.id)
        .map(|link| EvidenceRegressionFix {
            task_id: link.regressing.clone(),
            title: titles
                .get(link.regressing.as_str())
                .map(|fix| fix.title.clone())
                .unwrap_or_default(),
        })
        .collect();
    let paths = task
        .context_files
        .iter()
        .map(|selector| {
            let path = selector
                .split_once(':')
                .filter(|(kind, _)| matches!(*kind, "file" | "dir" | "symbol" | "glob"))
                .map_or(selector.as_str(), |(_, path)| path);
            path.split_once('#')
                .map_or(path, |(file, _)| file)
                .to_string()
        })
        .collect();
    EvidenceTask {
        task_id: task.id.clone(),
        title: task.title.clone(),
        occurrences,
        regression_fixes,
        paths,
    }
}
