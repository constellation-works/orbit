use std::collections::BTreeSet;

use chrono::{Duration, Utc};
use orbit_automation::condition::{
    CheckedCondition, ConditionEvaluator, TaskFacts, own_task_ids, self_facts,
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::automation::{CONDITION_SUBJECTS_PER_PASS, ConditionScope};

use super::super::condition::facts::{self, FactInputs};
use super::super::condition::{Target, condition_consumer, condition_status};
use super::WORKSPACE_SUBJECT;
use super::report::{
    AutomationExplain, ConditionExplain, ExplainCrossReads, ExplainOwnership, ExplainPrefilter,
    ExplainSubject, ExplainSubjects, ExplainedDefinition,
};
use super::subjects::{Report, SubjectState, subject_tree};
use crate::OrbitRuntime;

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
        let ownership = super::super::ownership::resolve(self, None);
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
