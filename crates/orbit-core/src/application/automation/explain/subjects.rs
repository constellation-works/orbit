use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{CheckedCondition, ConditionEvaluator, TaskFacts, Truth};
use orbit_types::workflow::automation::ConditionState;

use super::super::condition::ConditionSubjectView;
use super::MAX_LISTED;
use super::report::{ExplainMatch, ExplainMiss, ExplainSubject};

pub(super) fn subject_tree(
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

pub(super) struct SubjectState<'a> {
    pub(super) state: Option<&'a ConditionState>,
    pub(super) hold: Duration,
    pub(super) now: DateTime<Utc>,
}

#[derive(Default)]
pub(super) struct Report {
    pub(super) evaluated: usize,
    pub(super) matched: usize,
    pub(super) missed: usize,
    pub(super) unknown: usize,
    pub(super) matches: Vec<ExplainMatch>,
    pub(super) closest_misses: Vec<ExplainMiss>,
}

impl Report {
    pub(super) fn record(
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
