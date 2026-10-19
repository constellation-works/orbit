//! The scorecard rows: per orchestrator, each component's count over the
//! tasks it owns in the window, normalized per 100 owned tasks and per 8
//! active hours, with the composite and the tasks behind every count.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::{TaskComplexity, TaskOrigin};
use serde_json::{Map, Value, json};

use super::facts::{Cause, Component, Occurrence, OrchestratorFacts, TaskFacts};

/// The component groups, in the order the scorecard shows them.
pub(super) const GROUPS: &[(&str, &str)] = &[
    ("throughput", "Throughput"),
    ("proactivity", "Proactivity"),
    ("spec_defects", "Spec defects"),
    ("rework", "Rework"),
];

/// A counted column: its key, group, label and definition.
pub(super) struct ComponentSpec {
    pub(super) key: &'static str,
    pub(super) group: &'static str,
    pub(super) label: &'static str,
    pub(super) definition: &'static str,
}

/// Every counted column, components before the composite. A group's total
/// leads its parts.
pub(super) const COMPONENTS: &[ComponentSpec] = &[
    ComponentSpec {
        key: "owned",
        group: "throughput",
        label: "Owned",
        definition: "Tasks whose orchestrator attribution names this orchestrator, created in the window.",
    },
    ComponentSpec {
        key: "landed",
        group: "throughput",
        label: "Landed",
        definition: "Owned tasks that are done, or were done before they were archived.",
    },
    ComponentSpec {
        key: "weighted_landed",
        group: "throughput",
        label: "Weighted landed",
        definition: "Landed tasks weighted by complexity: low 1, medium 2, hard 4, xhard 8, unassessed or unset 1.",
    },
    ComponentSpec {
        key: "landed_initiative",
        group: "proactivity",
        label: "Landed initiative",
        definition: "Landed tasks whose typed origin is orchestrator_initiative.",
    },
    ComponentSpec {
        key: "dropped_initiative",
        group: "proactivity",
        label: "Dropped initiative",
        definition: "orchestrator_initiative tasks rejected or archived without ever landing: duplicates or noise.",
    },
    ComponentSpec {
        key: "net_initiative",
        group: "proactivity",
        label: "Net initiative",
        definition: "Landed initiative minus dropped initiative.",
    },
    ComponentSpec {
        key: "unknown_origin",
        group: "proactivity",
        label: "Unknown origin",
        definition: "Owned tasks with no typed origin. They are counted here and never guessed into another origin.",
    },
    ComponentSpec {
        key: "spec_defects",
        group: "spec_defects",
        label: "Spec defects",
        definition: "The sum of the four spec-defect sources.",
    },
    ComponentSpec {
        key: "spec_edits",
        group: "spec_defects",
        label: "Edited after run start",
        definition: "Tasks whose description or acceptance criteria were edited after their first implementation run started, from the documents each task-field history entry names. Edits before the first run do not count; entries written before history named the edited documents cannot be told apart and are not counted.",
    },
    ComponentSpec {
        key: "criteria_blocks",
        group: "spec_defects",
        label: "Criteria blocks",
        definition: "Agent blocks (task_blocked_by_agent) whose kind names a criterion, acceptance, spec, requirement, scope, ambiguity or contradiction, such as acceptance-criterion-unmet.",
    },
    ComponentSpec {
        key: "criteria_escalations",
        group: "spec_defects",
        label: "Criteria escalations",
        definition: "Final-recovery escalations whose diagnosis cites the acceptance criteria.",
    },
    ComponentSpec {
        key: "spec_regressions",
        group: "spec_defects",
        label: "Spec regressions",
        definition: "regression_from links against owned tasks whose typed classification has nature incomplete or escape spec_ambiguous.",
    },
    ComponentSpec {
        key: "rework",
        group: "rework",
        label: "Rework",
        definition: "The sum of the four rework sources, each attributed to spec, crew, infrastructure or other.",
    },
    ComponentSpec {
        key: "review_rejects",
        group: "rework",
        label: "Review rejects",
        definition: "Review gate settlements with verdict reject, and gate escalations of reviewer fixes that failed owner revalidation. Attributed to the crew.",
    },
    ComponentSpec {
        key: "pr_rework",
        group: "rework",
        label: "Blocked or unmerged PRs",
        definition: "[BLOCKED] PRs a failure handoff published, attributed like the block that published them, and delivery PRs a rejected, archived or landed task recorded but never merged.",
    },
    ComponentSpec {
        key: "repeat_runs",
        group: "rework",
        label: "Repeat runs",
        definition: "Implementation runs after a task's first, attributed to spec when anything since the previous run changed or questioned the criteria, else to the cause of the status change that ended it.",
    },
    ComponentSpec {
        key: "operator_requeues",
        group: "rework",
        label: "Operator requeues",
        definition: "Blocked tasks returned to the backlog by `orbit task requeue` or by a person's or agent session's status write, attributed like the block they cleared.",
    },
];

/// Complexity weights for weighted landed tasks; unassessed and unset weigh 1.
pub(super) const COMPLEXITY_WEIGHTS: &[(&str, i64)] = &[
    ("low", 1),
    ("medium", 2),
    ("hard", 4),
    ("xhard", 8),
    ("unassessed", 1),
];

/// The composite's terms: component key and weight. One table, shown with
/// the scorecard.
pub(super) const COMPOSITE_WEIGHTS: &[(&str, f64)] = &[
    ("weighted_landed", 1.0),
    ("landed_initiative", 1.0),
    ("spec_defects", -1.0),
    ("rework", -1.0),
];

/// Hours in one normalization window of active time.
const WINDOW_HOURS: f64 = 8.0;

fn complexity_weight(complexity: Option<TaskComplexity>) -> i64 {
    let name = complexity.map_or("unassessed", TaskComplexity::as_str);
    COMPLEXITY_WEIGHTS
        .iter()
        .find(|(key, _)| *key == name)
        .map_or(1, |(_, weight)| *weight)
}

fn component_key(component: Component) -> &'static str {
    match component {
        Component::SpecEdit => "spec_edits",
        Component::CriteriaBlock => "criteria_blocks",
        Component::CriteriaEscalation => "criteria_escalations",
        Component::SpecRegression => "spec_regressions",
        Component::ReviewReject => "review_rejects",
        Component::PrRework => "pr_rework",
        Component::RepeatRun => "repeat_runs",
        Component::OperatorRequeue => "operator_requeues",
    }
}

fn is_spec_defect(component: Component) -> bool {
    matches!(
        component,
        Component::SpecEdit
            | Component::CriteriaBlock
            | Component::CriteriaEscalation
            | Component::SpecRegression
    )
}

/// A component's count on one task and the occurrences behind it.
type Contributions<'a> = BTreeMap<&'static str, (i64, Vec<&'a Occurrence>)>;

fn bump<'a>(
    out: &mut Contributions<'a>,
    key: &'static str,
    count: i64,
    occurrence: Option<&'a Occurrence>,
) {
    let entry = out.entry(key).or_default();
    entry.0 += count;
    entry.1.extend(occurrence);
}

/// One task's contribution to each component, with the occurrences behind it.
fn contributions(task: &TaskFacts) -> Contributions<'_> {
    let mut out = Contributions::new();
    let initiative = task.origin == Some(TaskOrigin::OrchestratorInitiative);
    bump(&mut out, "owned", 1, None);
    if task.landed {
        bump(&mut out, "landed", 1, None);
        bump(
            &mut out,
            "weighted_landed",
            complexity_weight(task.complexity),
            None,
        );
    }
    if initiative && task.landed {
        bump(&mut out, "landed_initiative", 1, None);
        bump(&mut out, "net_initiative", 1, None);
    }
    if initiative && task.dropped {
        bump(&mut out, "dropped_initiative", 1, None);
        bump(&mut out, "net_initiative", -1, None);
    }
    if task.origin.is_none() {
        bump(&mut out, "unknown_origin", 1, None);
    }
    let mut edited = false;
    for occurrence in &task.occurrences {
        // An edited task counts once, however many edits it had.
        let count = match occurrence.component {
            Component::SpecEdit if edited => 0,
            Component::SpecEdit => {
                edited = true;
                1
            }
            _ => 1,
        };
        bump(
            &mut out,
            component_key(occurrence.component),
            count,
            Some(occurrence),
        );
        if is_spec_defect(occurrence.component) {
            bump(&mut out, "spec_defects", count, Some(occurrence));
        } else {
            bump(&mut out, "rework", count, Some(occurrence));
        }
    }
    out
}

/// Total length of the union of `intervals`, in hours.
fn union_hours(mut intervals: Vec<(DateTime<Utc>, DateTime<Utc>)>) -> f64 {
    intervals.retain(|(start, end)| end > start);
    intervals.sort();
    let mut total = chrono::Duration::zero();
    let mut current: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    for (start, end) in intervals {
        match current {
            Some((open, close)) if start <= close => current = Some((open, close.max(end))),
            Some((open, close)) => {
                total += close - open;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((open, close)) = current {
        total += close - open;
    }
    total.num_seconds() as f64 / 3600.0
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// One orchestrator's tasks in the window, and the row they make.
pub(super) struct Scored<'a> {
    pub(super) name: &'a str,
    pub(super) tasks: Vec<&'a TaskFacts>,
    pub(super) row: Value,
}

/// Every orchestrator with an owned task in the window, scored. Rows meeting
/// `min_support` owned tasks lead, then by owned tasks and name.
pub(super) fn score<'a>(
    facts: &'a OrchestratorFacts,
    since: Option<DateTime<Utc>>,
    min_support: usize,
) -> Vec<Scored<'a>> {
    let mut by_orchestrator: BTreeMap<&str, Vec<&TaskFacts>> = BTreeMap::new();
    for task in &facts.tasks {
        if since.is_none_or(|since| task.created_at >= since) {
            by_orchestrator
                .entry(task.orchestrator.as_str())
                .or_default()
                .push(task);
        }
    }
    let mut scored: Vec<Scored<'a>> = by_orchestrator
        .into_iter()
        .map(|(name, tasks)| {
            let row = row(name, &tasks, facts.computed_at, min_support);
            Scored { name, tasks, row }
        })
        .collect();
    scored.sort_by(|left, right| {
        let below = |scored: &Scored<'_>| scored.row["below_support"] == true;
        below(left)
            .cmp(&below(right))
            .then(right.tasks.len().cmp(&left.tasks.len()))
            .then(left.name.cmp(right.name))
    });
    scored
}

fn row(name: &str, tasks: &[&TaskFacts], now: DateTime<Utc>, min_support: usize) -> Value {
    let mut counts: BTreeMap<&'static str, i64> =
        COMPONENTS.iter().map(|spec| (spec.key, 0)).collect();
    let mut by_cause: BTreeMap<Cause, i64> = Cause::ALL.iter().map(|cause| (*cause, 0)).collect();
    let mut origins: BTreeMap<&'static str, i64> = BTreeMap::new();
    let mut intervals = Vec::new();
    let mut with_runs = 0;
    for task in tasks {
        for (key, (count, _)) in contributions(task) {
            *counts.entry(key).or_default() += count;
        }
        for occurrence in &task.occurrences {
            if let Some(cause) = occurrence.cause {
                *by_cause.entry(cause).or_default() += 1;
            }
        }
        *origins
            .entry(task.origin.map_or("unknown", TaskOrigin::as_str))
            .or_default() += 1;
        if let (Some(first), Some(last)) = (task.runs.first(), task.runs.last()) {
            with_runs += 1;
            intervals.push((first.start, last.end.unwrap_or(now)));
        }
    }
    let owned = tasks.len() as f64;
    let active_hours = union_hours(intervals);
    let composite: f64 = COMPOSITE_WEIGHTS
        .iter()
        .map(|(key, weight)| weight * counts.get(key).copied().unwrap_or(0) as f64)
        .sum();
    let per_100 = |value: f64| round(value * 100.0 / owned);
    let per_8h =
        |value: f64| (active_hours > 0.0).then(|| round(value * WINDOW_HOURS / active_hours));
    let mut per_100_tasks = Map::new();
    let mut per_8_hours = Map::new();
    for (key, count) in &counts {
        per_100_tasks.insert((*key).into(), json!(per_100(*count as f64)));
        per_8_hours.insert((*key).into(), json!(per_8h(*count as f64)));
    }
    let initiative = counts["landed_initiative"] + counts["dropped_initiative"];
    json!({
        "orchestrator": name,
        "below_support": tasks.len() < min_support,
        "support": {
            "owned_tasks": tasks.len(),
            "tasks_with_runs": with_runs,
            "active_hours": round(active_hours),
            "active_8h_windows": round(active_hours / WINDOW_HOURS),
        },
        "counts": counts,
        "per_100_tasks": per_100_tasks,
        "per_8_hours": per_8_hours,
        "landed_share": (initiative > 0)
            .then(|| round(counts["landed_initiative"] as f64 / initiative as f64)),
        "origins": {
            "operator_request": origins.get("operator_request").copied().unwrap_or(0),
            "orchestrator_initiative": origins.get("orchestrator_initiative").copied().unwrap_or(0),
            "automation": origins.get("automation").copied().unwrap_or(0),
            "unknown": origins.get("unknown").copied().unwrap_or(0),
        },
        "rework_by_cause": by_cause
            .iter()
            .map(|(cause, count)| (cause.as_str().to_string(), json!(count)))
            .collect::<Map<_, _>>(),
        "composite": {
            "value": round(composite),
            "per_100_tasks": per_100(composite),
            "per_8_hours": per_8h(composite),
        },
    })
}

/// Each component's count and the owned tasks behind it, in the scorecard's
/// column order; a task appears under a component only when it contributes.
pub(super) fn drill_down(tasks: &[&TaskFacts]) -> Vec<Value> {
    let per_task: Vec<_> = tasks
        .iter()
        .map(|task| (*task, contributions(task)))
        .collect();
    COMPONENTS
        .iter()
        .map(|spec| {
            let mut rows: Vec<(&TaskFacts, i64, &Vec<&Occurrence>)> = per_task
                .iter()
                .filter_map(|(task, contributions)| {
                    let (count, occurrences) = contributions.get(spec.key)?;
                    (*count != 0 || !occurrences.is_empty()).then_some((*task, *count, occurrences))
                })
                .collect();
            rows.sort_by(|left, right| {
                right
                    .1
                    .abs()
                    .cmp(&left.1.abs())
                    .then(right.0.created_at.cmp(&left.0.created_at))
            });
            json!({
                "key": spec.key,
                "group": spec.group,
                "label": spec.label,
                "count": rows.iter().map(|(_, count, _)| count).sum::<i64>(),
                "tasks": rows.iter().map(|(task, count, occurrences)| json!({
                    "task_id": task.id,
                    "title": task.title,
                    "status": task.status.to_string(),
                    "origin": task.origin.map(TaskOrigin::as_str),
                    "complexity": task.complexity.map(TaskComplexity::as_str),
                    "created_at": task.created_at.to_rfc3339(),
                    "count": count,
                    "occurrences": occurrences.iter().map(|occurrence| json!({
                        "at": occurrence.at.to_rfc3339(),
                        "detail": occurrence.detail,
                        "cause": occurrence.cause.map(Cause::as_str),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The definitions every response carries: groups, components, causes,
/// complexity and composite weights.
pub(super) fn definitions() -> Value {
    json!({
        "groups": GROUPS.iter().map(|(key, label)| json!({"key": key, "label": label})).collect::<Vec<_>>(),
        "components": COMPONENTS.iter().map(|spec| json!({
            "key": spec.key,
            "group": spec.group,
            "label": spec.label,
            "definition": spec.definition,
        })).collect::<Vec<_>>(),
        "causes": [
            {"key": "spec", "definition": "The task's specification: an edit to its description or criteria, or a criteria block or escalation, preceded the rework."},
            {"key": "crew", "definition": "The crew's work: a run failure or a review rejection."},
            {"key": "infrastructure", "definition": "An infrastructure failure the run-failure classification names: auth, sandbox, disk, provider limits or outages, interruptions."},
            {"key": "other", "definition": "Base conflicts, manual blocks, agent blockers that do not name the criteria, and causes nothing records."},
        ],
        "complexity_weights": COMPLEXITY_WEIGHTS.iter().map(|(key, weight)| (key.to_string(), json!(weight))).collect::<Map<_, _>>(),
        "composite": {
            "definition": "Weighted landed + landed initiative − spec defects − rework. Shown after the components and normalized like them.",
            "weights": COMPOSITE_WEIGHTS.iter().map(|(key, weight)| json!({"key": key, "weight": weight})).collect::<Vec<_>>(),
        },
        "normalization": {
            "per_100_tasks": "Count × 100 ÷ owned tasks: compares quality across orchestrators of different volume.",
            "per_8_hours": "Count × 8 ÷ active hours: compares throughput. Active time is the union, over owned tasks, of the span from a task's first run start to its last run end (now while a run is open).",
        },
    })
}
