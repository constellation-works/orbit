//! `orbit automation explain` [ORB-19990]: evaluate one definition's
//! condition over live workspace state and say what matches and why, beside
//! its durable state: arm states and holds, the stall, recent fires with
//! their upstream chain, and `self`. Nothing is written
//! (`docs/design/condition-triggers/1_scope.md` §9).
//!
//! `job:<name>` explains a job's auto-task steps instead ([`job`]).

mod job;

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{
    CheckedCondition, ClauseNode, ConditionEvaluator, TaskFacts, Truth, own_task_ids, self_facts,
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::automation::{
    CONDITION_SUBJECTS_PER_PASS, CONDITION_WORKSPACE_SUBJECT, ConditionScope, ConditionState,
};
use serde::Serialize;

use super::condition::facts::{self, FactInputs};
use super::condition::{
    ConditionStatus, ConditionSubjectView, Target, condition_consumer, condition_status,
};
use crate::OrbitRuntime;
pub use job::JobStepsExplain;

/// Most matches and closest misses one explain lists.
const MAX_LISTED: usize = 20;

/// The subject key of a workspace-scope condition.
const WORKSPACE_SUBJECT: &str = CONDITION_WORKSPACE_SUBJECT;

/// The `orbit.automation.explain` payload: a condition's report, or a job's
/// auto-task steps for `job:<name>`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AutomationExplain {
    Condition(Box<ConditionExplain>),
    Job(Box<JobStepsExplain>),
}

/// The explain report for a routine's or auto-task's condition.
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
    /// The durable state: ownership, arm states and holds, the stall, recent
    /// fires with their upstream chain, and `self`.
    pub tracking: ConditionStatus,
}

#[derive(Debug, Serialize)]
pub struct ExplainedDefinition {
    /// `routine`, `auto-task` or `job`.
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
    /// `armed`, `fired`, or `untracked` before the first pass records it.
    pub arm: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_since: Option<DateTime<Utc>>,
    /// Minutes of `for_minutes` still to hold before a fire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_remaining_minutes: Option<i64>,
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

impl OrbitRuntime {
    /// Explain `definition`: `job:<name>` reports that job's auto-task
    /// steps, anything else one condition definition. Read-only.
    pub fn explain_automation(
        &self,
        definition: &str,
        subject: Option<&str>,
    ) -> Result<AutomationExplain, OrbitError> {
        let Some(job) = definition.trim().strip_prefix("job:") else {
            return self
                .explain_automation_condition(definition, subject)
                .map(|report| AutomationExplain::Condition(Box::new(report)));
        };
        if subject.is_some_and(|subject| !subject.trim().is_empty()) {
            return Err(OrbitError::InvalidInput(
                "a job explain has no subject; drop --subject".into(),
            ));
        }
        if job.is_empty() {
            return Err(OrbitError::InvalidInput(
                "definition must name a job after job:".into(),
            ));
        }
        self.explain_job_auto_task_steps(job)
            .map(|report| AutomationExplain::Job(Box::new(report)))
    }

    /// Explain one condition definition over live state. `definition` is a
    /// name, or `routine:<name>` / `auto-task:<name>`; `subject` picks the
    /// clause tree's subject. Read-only.
    fn explain_automation_condition(
        &self,
        definition: &str,
        subject: Option<&str>,
    ) -> Result<ConditionExplain, OrbitError> {
        let found = self.condition_definition_for(definition)?;
        let condition = found.condition_definition()?;
        let checked =
            orbit_automation::condition::check_condition(condition.condition, found.carrier)
                .map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "{} '{}' does not load ({}): {error}",
                        found.carrier.kind(),
                        found.name,
                        found.path
                    ))
                })?;
        let now = Utc::now();
        let consumer = condition_consumer(self, found.carrier, &found.name)?;
        let state = self.automation_store()?.condition_state(&consumer)?;
        // Explain sees what the next pass would: the definition's own minted
        // tasks are neither subjects nor, unless `board_includes_own`, board.
        let own = match condition.target {
            Target::Mint(_) => own_task_ids(state.as_ref()),
            Target::Job(_) => BTreeSet::new(),
        };
        let own_tag = match condition.target {
            Target::Mint(definition) => {
                Some(orbit_types::workflow::auto_task_tag(&definition.name))
            }
            Target::Job(_) => None,
        };
        let inputs = FactInputs {
            own_tasks: if condition.condition.board_includes_own {
                BTreeSet::new()
            } else {
                own.clone()
            },
            self_facts: self_facts(state.as_ref(), now),
        };
        let gathered = facts::gather(self, &checked.reads, now, &inputs)?;
        let evaluator = ConditionEvaluator::new(&gathered.facts);
        let tracking = condition_status(self, &condition, now)?;
        let subject_state = SubjectState {
            state: state.as_ref(),
            hold: Duration::minutes(i64::try_from(condition.condition.for_minutes).unwrap_or(0)),
            now,
        };
        let mut report = Report::default();
        let mut selected = 1;
        let mut subjects = Vec::new();
        match checked.scope {
            ConditionScope::Task => {
                let prefiltered = facts::prefiltered(&gathered.tasks, &checked.prefilter)
                    .into_iter()
                    .filter(|task| {
                        !own.contains(&task.id)
                            && own_tag.as_ref().is_none_or(|tag| !task.tags.contains(tag))
                    })
                    .collect::<Vec<_>>();
                selected = prefiltered.len();
                for task in prefiltered.into_iter().take(CONDITION_SUBJECTS_PER_PASS) {
                    subjects.push(facts::task_facts(self, task, &checked.reads)?.0);
                }
                for task in &subjects {
                    report.record(
                        &evaluator,
                        &checked,
                        Some(task),
                        &task.id,
                        Some(&task.title),
                        &subject_state,
                    );
                }
            }
            _ => report.record(
                &evaluator,
                &checked,
                None,
                WORKSPACE_SUBJECT,
                None,
                &subject_state,
            ),
        }
        let subject =
            self.explained_subject(&evaluator, &checked, &gathered, &subjects, &report, subject)?;
        Ok(ConditionExplain {
            definition: ExplainedDefinition {
                kind: found.carrier.kind(),
                name: found.name.clone(),
                path: found.path.clone(),
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
                selected,
                evaluated: report.evaluated,
                truncated: selected > report.evaluated,
                matched: report.matched,
                missed: report.missed,
                unknown: report.unknown,
            },
            matches: report.matches,
            closest_misses: report.closest_misses,
            subject,
            unavailable: gathered.unavailable,
            tracking,
        })
    }

    /// Workspace and task conditions are the owner checkout's (§4).
    fn condition_ownership(&self) -> ExplainOwnership {
        let ownership = super::ownership::resolve(self, None);
        let replica = self.coordination_write_owner().is_some();
        ExplainOwnership {
            verdict: if replica {
                "replica_checkout"
            } else {
                ownership.refusal().unwrap_or("owned_here")
            },
            owner_machine: ownership.owner_machine,
            this_host: self.automation_machine_identity().map(str::to_owned),
            replica,
        }
    }

    fn explained_subject(
        &self,
        evaluator: &ConditionEvaluator,
        checked: &CheckedCondition,
        gathered: &facts::GatheredFacts,
        subjects: &[TaskFacts],
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
                .or_else(|| subjects.first().map(|task| task.id.clone()))
            {
                Some(key) => key,
                None => return Ok(None),
            },
        };
        let task = match subjects.iter().find(|task| task.id == key) {
            Some(task) => task.clone(),
            None => {
                let task = gathered
                    .tasks
                    .get(&key)
                    .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, key.as_str()))?;
                facts::task_facts(self, task, &checked.reads)?.0
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

struct SubjectState<'a> {
    state: Option<&'a ConditionState>,
    hold: Duration,
    now: DateTime<Utc>,
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
        subject_state: &SubjectState<'_>,
    ) {
        self.evaluated += 1;
        match evaluator.truth(&checked.when, task) {
            Truth::True => {
                self.matched += 1;
                if self.matches.len() < MAX_LISTED {
                    let tracked = subject_state
                        .state
                        .and_then(|state| state.subjects.get(key))
                        .map(|subject| {
                            ConditionSubjectView::from_subject(
                                key,
                                subject,
                                subject_state.hold,
                                subject_state.now,
                            )
                        });
                    self.matches.push(ExplainMatch {
                        subject: key.to_string(),
                        title: title.cloned(),
                        arm: tracked.as_ref().map_or(
                            if subject_state.state.is_some() {
                                "armed"
                            } else {
                                "untracked"
                            },
                            |subject| subject.arm,
                        ),
                        true_since: tracked.as_ref().and_then(|subject| subject.true_since),
                        hold_remaining_minutes: tracked
                            .as_ref()
                            .and_then(|subject| subject.hold_remaining_minutes),
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
