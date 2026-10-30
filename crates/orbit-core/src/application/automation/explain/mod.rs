//! `orbit automation explain` [ORB-19990]: evaluate one definition's
//! condition over live workspace state and say what matches and why, without
//! writing anything (`docs/design/condition-triggers/1_scope.md` §9).
//!
//! Conditions do not fire in this release, so there is no arm state, hold,
//! fire record or `self` history to show; the report says so instead of
//! inventing values.

mod facts;

use chrono::Utc;
use orbit_automation::condition::{
    CheckedCondition, ClauseNode, ConditionCarrier, ConditionEvaluator, TaskFacts, Truth,
    check_condition,
};
use orbit_automation::routines::loader::find_routine_definition;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::AutoTaskSchedule;
use orbit_types::workflow::automation::{ConditionScope, ConditionTrigger};
use serde::Serialize;

use crate::OrbitRuntime;

/// Most matches and closest misses one explain lists.
const MAX_LISTED: usize = 20;

/// The subject key of a workspace-scope condition.
const WORKSPACE_SUBJECT: &str = "workspace";

/// The explain report, the `orbit.automation.explain` payload.
#[derive(Debug, Serialize)]
pub struct ConditionExplain {
    pub definition: ExplainedDefinition,
    pub scope: &'static str,
    /// Where the condition sits in its file.
    pub clause: &'static str,
    pub when: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rearm: Option<String>,
    /// Task scope: the store query subjects come from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefilter: Option<ExplainPrefilter>,
    pub cross_reads: ExplainCrossReads,
    pub ownership: ExplainOwnership,
    pub evaluated_at: String,
    pub subjects: ExplainSubjects,
    pub matches: Vec<ExplainMatch>,
    pub closest_misses: Vec<ExplainMiss>,
    /// The clause tree for one subject; absent when nothing was selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<ExplainSubject>,
    /// Values the board could not read; expressions reading them are unknown.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<String>,
    pub tracking: ExplainTracking,
}

#[derive(Debug, Serialize)]
pub struct ExplainedDefinition {
    /// `routine` or `auto-task`.
    pub kind: &'static str,
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ExplainPrefilter {
    pub statuses: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainCrossReads {
    pub tasks: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainOwnership {
    /// `owned_here`, `owned_elsewhere` or `ownership_unresolved`.
    pub verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_machine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub this_host: Option<String>,
    /// Whether this checkout is a replica, which never evaluates a
    /// workspace or task condition.
    pub replica: bool,
}

#[derive(Debug, Serialize)]
pub struct ExplainSubjects {
    /// Subjects the scope selected (task scope: the prefilter's tasks).
    pub selected: usize,
    /// Subjects evaluated, at most 200.
    pub evaluated: usize,
    /// Whether the cap left selected subjects unevaluated.
    pub truncated: bool,
    pub matched: usize,
    pub missed: usize,
    pub unknown: usize,
}

#[derive(Debug, Serialize)]
pub struct ExplainMatch {
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainMiss {
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The one top-level clause that is not true.
    pub clause: String,
    /// `false` or `unknown`.
    pub value: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainSubject {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Task scope: whether the prefilter selects this task. A task outside
    /// it is never a subject, whatever `when` says.
    pub in_prefilter: bool,
    pub value: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub when: ClauseNode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rearm: Option<ClauseNode>,
}

/// What firing would track. Nothing is tracked until conditions fire.
#[derive(Debug, Serialize)]
pub struct ExplainTracking {
    pub arm_state: &'static str,
    pub holds: &'static str,
    pub fires: &'static str,
    #[serde(rename = "self")]
    pub self_reads: &'static str,
    pub note: &'static str,
}

const NOT_TRACKED: &str = "not_tracked";

impl OrbitRuntime {
    /// Explain one condition definition over live state. `definition` is a
    /// name, or `routine:<name>` / `auto-task:<name>`; `subject` picks the
    /// clause tree's subject. Read-only.
    pub fn explain_automation_condition(
        &self,
        definition: &str,
        subject: Option<&str>,
    ) -> Result<ConditionExplain, OrbitError> {
        let found = self.condition_definition(definition)?;
        let checked = check_condition(&found.condition, found.carrier).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{} '{}' does not load ({}): {error}",
                found.carrier.kind(),
                found.name,
                found.path
            ))
        })?;
        let now = Utc::now();
        let gathered = facts::gather(self, &checked, now)?;
        let evaluator = ConditionEvaluator::new(&gathered.facts);
        let mut report = Report::default();
        match checked.scope {
            ConditionScope::Task => {
                for task in &gathered.subjects {
                    report.record(
                        &evaluator,
                        &checked,
                        Some(task),
                        &task.id,
                        Some(&task.title),
                    );
                }
            }
            _ => report.record(&evaluator, &checked, None, WORKSPACE_SUBJECT, None),
        }
        let subject = self.explained_subject(&evaluator, &checked, &gathered, &report, subject)?;
        Ok(ConditionExplain {
            definition: ExplainedDefinition {
                kind: found.carrier.kind(),
                name: found.name,
                path: found.path,
                enabled: found.enabled,
            },
            scope: checked.scope.as_str(),
            clause: found.carrier.clause(),
            when: checked.when.source.clone(),
            rearm: checked.rearm.as_ref().map(|rearm| rearm.source.clone()),
            prefilter: (checked.scope == ConditionScope::Task).then(|| ExplainPrefilter {
                statuses: checked.prefilter.statuses.clone(),
                tags: checked.prefilter.tags.clone(),
            }),
            cross_reads: ExplainCrossReads {
                tasks: checked.reads.task_ids.iter().cloned().collect(),
                tags: checked.reads.tags.iter().cloned().collect(),
            },
            ownership: self.condition_ownership(),
            evaluated_at: now.to_rfc3339(),
            subjects: ExplainSubjects {
                selected: gathered.selected,
                evaluated: report.evaluated,
                truncated: gathered.selected > report.evaluated,
                matched: report.matched,
                missed: report.missed,
                unknown: report.unknown,
            },
            matches: report.matches,
            closest_misses: report.closest_misses,
            subject,
            unavailable: gathered.unavailable,
            tracking: ExplainTracking {
                arm_state: NOT_TRACKED,
                holds: NOT_TRACKED,
                fires: NOT_TRACKED,
                self_reads: NOT_TRACKED,
                note: "conditions do not fire in this Orbit: nothing is armed, held or fired, \
                       and self reads no fire history",
            },
        })
    }

    /// Find the named definition among this checkout's routines and
    /// auto-tasks and require that it carries a condition.
    fn condition_definition(&self, selector: &str) -> Result<FoundCondition, OrbitError> {
        let selector = selector.trim();
        let (kinds, name) = match selector.split_once(':') {
            Some(("routine", name)) => (&[ConditionCarrier::Routine][..], name),
            Some(("auto-task", name)) => (&[ConditionCarrier::AutoTask][..], name),
            _ => (
                &[ConditionCarrier::Routine, ConditionCarrier::AutoTask][..],
                selector,
            ),
        };
        if name.is_empty() {
            return Err(OrbitError::InvalidInput(
                "definition must name a routine or auto-task".into(),
            ));
        }
        let mut found = Vec::new();
        for carrier in kinds {
            if let Some(definition) = self.load_condition_carrier(*carrier, name)? {
                found.push(definition);
            }
        }
        match found.len() {
            0 => Err(OrbitError::InvalidInput(format!(
                "no {} named '{name}' in this workspace",
                match kinds {
                    [only] => only.kind(),
                    _ => "routine or auto-task",
                }
            ))),
            1 => {
                let carrier = found.remove(0);
                let condition = carrier.condition.ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "{} '{}' has no condition; explain covers {} triggers only",
                        carrier.carrier.kind(),
                        carrier.name,
                        carrier.carrier.clause()
                    ))
                })?;
                Ok(FoundCondition {
                    carrier: carrier.carrier,
                    name: carrier.name,
                    path: carrier.path,
                    enabled: carrier.enabled,
                    condition,
                })
            }
            _ => Err(OrbitError::InvalidInput(format!(
                "both a routine and an auto-task are named '{name}'; pass routine:{name} or \
                 auto-task:{name}"
            ))),
        }
    }

    fn load_condition_carrier(
        &self,
        carrier: ConditionCarrier,
        name: &str,
    ) -> Result<Option<LoadedCarrier>, OrbitError> {
        match carrier {
            ConditionCarrier::Routine => {
                let Some((path, loaded)) = find_routine_definition(&self.shared_root(), name)
                else {
                    return Ok(None);
                };
                let definition = loaded.map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "routine '{name}' does not load ({}): {error}",
                        path.display()
                    ))
                })?;
                Ok(Some(LoadedCarrier {
                    carrier,
                    name: definition.name,
                    path: path.display().to_string(),
                    enabled: definition.enabled,
                    condition: definition.trigger.condition,
                }))
            }
            ConditionCarrier::AutoTask => {
                let Some((_, path)) = self.auto_task_read_source(name)? else {
                    return Ok(None);
                };
                let definition = self.auto_task_show(name)?.ok_or_else(|| {
                    OrbitError::InvalidInput(format!("auto-task '{name}' is no longer defined"))
                })?;
                let condition = match definition.schedule {
                    AutoTaskSchedule::Condition { condition } => Some(condition),
                    _ => None,
                };
                Ok(Some(LoadedCarrier {
                    carrier,
                    name: definition.name,
                    path: path.display().to_string(),
                    enabled: definition.enabled,
                    condition,
                }))
            }
        }
    }

    /// Workspace and task conditions are the owner checkout's (§4).
    fn condition_ownership(&self) -> ExplainOwnership {
        let ownership = super::ownership::resolve(self, None);
        ExplainOwnership {
            verdict: ownership.refusal().unwrap_or("owned_here"),
            owner_machine: ownership.owner_machine,
            this_host: self.automation_machine_identity().map(str::to_owned),
            replica: self.coordination_write_owner().is_some(),
        }
    }

    fn explained_subject(
        &self,
        evaluator: &ConditionEvaluator,
        checked: &CheckedCondition,
        gathered: &facts::GatheredFacts,
        report: &Report,
        requested: Option<&str>,
    ) -> Result<Option<ExplainSubject>, OrbitError> {
        let requested = requested.map(str::trim).filter(|key| !key.is_empty());
        if checked.scope != ConditionScope::Task {
            if let Some(key) = requested
                && key != WORKSPACE_SUBJECT
            {
                return Err(OrbitError::InvalidInput(format!(
                    "a {} condition has one subject, '{WORKSPACE_SUBJECT}'; '{key}' is not it",
                    checked.scope.as_str()
                )));
            }
            return Ok(Some(subject_tree(
                evaluator,
                checked,
                None,
                WORKSPACE_SUBJECT,
                None,
                true,
            )));
        }
        let key = match requested {
            Some(key) => key.to_string(),
            None => match report
                .matches
                .first()
                .map(|matched| matched.subject.clone())
                .or_else(|| {
                    report
                        .closest_misses
                        .first()
                        .map(|miss| miss.subject.clone())
                })
                .or_else(|| gathered.subjects.first().map(|task| task.id.clone()))
            {
                Some(key) => key,
                None => return Ok(None),
            },
        };
        let task = match gathered.subjects.iter().find(|task| task.id == key) {
            Some(task) => task.clone(),
            None => {
                let task = gathered
                    .tasks
                    .get(&key)
                    .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, key.as_str()))?;
                facts::task_facts(self, task, checked.reads.status_since)?
            }
        };
        let in_prefilter = gathered
            .tasks
            .get(&key)
            .is_some_and(|task| facts::passes_prefilter(task, &checked.prefilter));
        Ok(Some(subject_tree(
            evaluator,
            checked,
            Some(&task),
            &key,
            Some(task.title.clone()),
            in_prefilter,
        )))
    }
}

/// A routine or auto-task by that name, with its condition if it has one.
struct LoadedCarrier {
    carrier: ConditionCarrier,
    name: String,
    path: String,
    enabled: bool,
    condition: Option<ConditionTrigger>,
}

struct FoundCondition {
    carrier: ConditionCarrier,
    name: String,
    path: String,
    enabled: bool,
    condition: ConditionTrigger,
}

fn subject_tree(
    evaluator: &ConditionEvaluator,
    checked: &CheckedCondition,
    task: Option<&TaskFacts>,
    key: &str,
    title: Option<String>,
    in_prefilter: bool,
) -> ExplainSubject {
    let truth = evaluator.truth(&checked.when, task);
    ExplainSubject {
        key: key.to_string(),
        title,
        in_prefilter,
        value: truth.label(),
        reason: unknown_reason(&truth),
        when: evaluator.clause_tree(&checked.when, task),
        rearm: checked
            .rearm
            .as_ref()
            .map(|rearm| evaluator.clause_tree(rearm, task)),
    }
}

fn unknown_reason(truth: &Truth) -> Option<String> {
    match truth {
        Truth::Unknown(reason) => Some(reason.clone()),
        _ => None,
    }
}

#[derive(Default)]
struct Report {
    evaluated: usize,
    matched: usize,
    missed: usize,
    unknown: usize,
    matches: Vec<ExplainMatch>,
    closest_misses: Vec<ExplainMiss>,
}

impl Report {
    fn record(
        &mut self,
        evaluator: &ConditionEvaluator,
        checked: &CheckedCondition,
        task: Option<&TaskFacts>,
        key: &str,
        title: Option<&String>,
    ) {
        self.evaluated += 1;
        match evaluator.truth(&checked.when, task) {
            Truth::True => {
                self.matched += 1;
                if self.matches.len() < MAX_LISTED {
                    self.matches.push(ExplainMatch {
                        subject: key.to_string(),
                        title: title.cloned(),
                    });
                }
                return;
            }
            Truth::False => self.missed += 1,
            Truth::Unknown(_) => self.unknown += 1,
        }
        if self.closest_misses.len() >= MAX_LISTED {
            return;
        }
        let failing = evaluator
            .top_level(&checked.when, task)
            .into_iter()
            .filter(|clause| !clause.value.is_true())
            .collect::<Vec<_>>();
        if let [clause] = failing.as_slice() {
            self.closest_misses.push(ExplainMiss {
                subject: key.to_string(),
                title: title.cloned(),
                clause: clause.text.clone(),
                value: clause.value.label(),
                reason: unknown_reason(&clause.value),
            });
        }
    }
}
