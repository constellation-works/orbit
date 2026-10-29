//! The hands a task met beyond its stored attribution fields: who piloted,
//! reviewed and recovered it, in time order, for the task detail's
//! **Handled by** row.
//!
//! Every hand comes from a record Orbit wrote, never from an agent message:
//!
//! - **piloted**: each task-pilot receipt the apply boundary sealed, with the
//!   crew and model its pilot run recorded;
//! - **reviewed**: each review gate settlement comment, with the reviewer it
//!   names and its verdict, and the attempt's run from the lineage ledger of
//!   the task's latest gate certificate (older lineages carry no run);
//! - **recovered**: a final-recovery decision that completed or requeued the
//!   task (its model from the recovery invocation of that run), a
//!   blocked-task triage requeue, an operator's claim recovery to the
//!   backlog, and an operator requeue.
//!
//! Nothing here is stored on the task; the stored `created_by`,
//! `planned_by`, `implemented_by` and `orchestrator` fields stay the
//! dashboard's own.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{InvocationQuery, TaskRow};
use orbit_types::identity::infer_agent_family_from_model;
use orbit_types::task::{
    FINAL_RECOVERY_DECIDED_EVENT, RecordedFinalRecovery, TaskDecision, TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate};
use serde_json::{Map, Value, json};

use super::facts::{FindingState, settlement};
use super::records::is_operator_requeue;
use super::roles::pilot_warnings;
use crate::OrbitRuntime;
use crate::application::codebase::actor_kind;
use crate::application::task::BLOCKED_TASK_TRIAGED_EVENT;

/// A hand's verb on the task.
const PILOTED: &str = "piloted";
const REVIEWED: &str = "reviewed";
const RECOVERED: &str = "recovered";
/// The decision a blocked-task triage requeue records on its final-recovery
/// event, telling it apart from final recovery's own requeue.
const TRIAGE_REQUEUE_DECISION: &str = "blocked_task_triage_requeue";
/// The lead of the triage comment, whose header names who triaged.
const TRIAGE_COMMENT_LEAD: &str = "blocked_task_triage ";
/// Claim recovery's history event: the only claim mutation that records it.
const CLAIM_REVOKED_EVENT: &str = "claim_revoked";
/// Who a human recovery is shown as.
const OPERATOR: &str = "operator";
/// How many of a recovery run's invocations are read for its model.
const RECOVERY_INVOCATION_LIMIT: usize = 50;

/// One derived hand.
#[derive(Debug, Clone, PartialEq)]
struct Hand {
    verb: &'static str,
    crew: Option<String>,
    model: Option<String>,
    /// Who acted when neither a crew nor a model names it: `operator` or the
    /// recorded actor label.
    actor: Option<String>,
    at: DateTime<Utc>,
    run_id: Option<String>,
    /// The review verdict, for a reviewed hand.
    verdict: Option<String>,
    /// How many findings the reviewer fixed, for a reviewed hand.
    fixed: usize,
    /// Which record a recovered hand comes from: `final_recovery`,
    /// `blocked_task_triage`, `claim_recovery` or `operator_requeue`.
    via: Option<&'static str>,
}

impl Hand {
    fn new(verb: &'static str, at: DateTime<Utc>) -> Self {
        Self {
            verb,
            crew: None,
            model: None,
            actor: None,
            at,
            run_id: None,
            verdict: None,
            fixed: 0,
            via: None,
        }
    }

    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("verb".into(), json!(self.verb));
        object.insert("crew".into(), json!(self.crew));
        object.insert("model".into(), json!(self.model));
        // The agent family the model or actor names, for the hand's colour.
        let family = [self.model.as_deref(), self.actor.as_deref()]
            .into_iter()
            .flatten()
            .find_map(infer_agent_family_from_model);
        if let Some(family) = family {
            object.insert("family".into(), json!(family));
        }
        object.insert(
            "at".into(),
            json!(self.at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        );
        if let Some(actor) = &self.actor {
            object.insert("actor".into(), json!(actor));
        }
        if let Some(run_id) = &self.run_id {
            object.insert("run_id".into(), json!(run_id));
        }
        if let Some(verdict) = &self.verdict {
            object.insert("verdict".into(), json!(verdict));
        }
        if self.fixed > 0 {
            object.insert("fixed_findings".into(), json!(self.fixed));
        }
        if let Some(via) = self.via {
            object.insert("via".into(), json!(via));
        }
        Value::Object(object)
    }
}

impl OrbitRuntime {
    /// The task's piloted, reviewed and recovered hands, oldest first, as
    /// the task detail's `handled_by` list. A record whose run cannot be
    /// read still yields its hand, without the crew or model that run would
    /// have named.
    pub fn task_handled_by(&self, row: &TaskRow) -> Vec<Value> {
        let mut hands = self.piloted_hands(row);
        hands.extend(self.reviewed_hands(row));
        hands.extend(self.recovered_hands(row));
        hands.sort_by_key(|hand| hand.at);
        hands.iter().map(Hand::to_json).collect()
    }

    fn piloted_hands(&self, row: &TaskRow) -> Vec<Hand> {
        pilot_warnings::settle(&row.task, &row.history, &row.comments)
            .assessments
            .into_iter()
            .map(|assessment| {
                let mut hand = Hand::new(PILOTED, assessment.at);
                if let Some(run_id) = assessment.run_id {
                    if let Some(run) = self.readable_run(&run_id) {
                        hand.crew = run.resolved_crew;
                        hand.model = run.crew_model;
                    }
                    hand.run_id = Some(run_id);
                }
                hand
            })
            .collect()
    }

    fn reviewed_hands(&self, row: &TaskRow) -> Vec<Hand> {
        let mut seen = BTreeSet::new();
        let settlements: Vec<_> = row
            .comments
            .iter()
            .filter_map(settlement)
            .filter(|review| seen.insert(review.attempt_id.clone()))
            .collect();
        if settlements.is_empty() {
            return Vec::new();
        }
        let runs = self.review_attempt_runs(row);
        settlements
            .into_iter()
            .map(|review| {
                let mut hand = Hand::new(REVIEWED, review.at);
                hand.crew = review.reviewer.crew;
                hand.model = review.reviewer.model;
                hand.verdict = Some(review.verdict.as_str().to_string());
                hand.fixed = review
                    .findings
                    .iter()
                    .filter(|finding| finding.state == FindingState::Fixed)
                    .count();
                hand.run_id = runs.get(&review.attempt_id).cloned();
                hand
            })
            .collect()
    }

    /// Each attempt's run in the lineage of the task's latest gate
    /// certificate. Empty when the task has none or it cannot be read.
    fn review_attempt_runs(&self, row: &TaskRow) -> HashMap<String, String> {
        if !row
            .artifacts
            .iter()
            .any(|file| file.path == REVIEW_GATE_ARTIFACT)
        {
            return HashMap::new();
        }
        let read = || -> Result<HashMap<String, String>, OrbitError> {
            let Some(artifact) = self.get_task_artifact(&row.task.id, REVIEW_GATE_ARTIFACT)? else {
                return Ok(HashMap::new());
            };
            let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content)
            else {
                return Ok(HashMap::new());
            };
            let ledger = self
                .review_store()?
                .review_ledger(&self.workspace_id()?, &certificate.lineage_key)?;
            Ok(ledger
                .map(|ledger| {
                    ledger
                        .attempts
                        .into_iter()
                        .map(|attempt| (attempt.attempt_id, attempt.run_id))
                        .collect()
                })
                .unwrap_or_default())
        };
        read().unwrap_or_else(|error| {
            tracing::warn!(task_id = %row.task.id, %error, "handled by: review ledger unreadable");
            HashMap::new()
        })
    }

    fn recovered_hands(&self, row: &TaskRow) -> Vec<Hand> {
        let mut hands = Vec::new();
        for entry in &row.history {
            if let Some(hand) = self.final_recovery_hand(entry) {
                hands.push(hand);
            } else if entry.event == BLOCKED_TASK_TRIAGED_EVENT && triage_requeued(entry) {
                hands.push(triage_hand(row, entry));
            } else if entry.event == CLAIM_REVOKED_EVENT
                && entry.to_status == Some(TaskStatus::Backlog)
            {
                hands.push(human_recovery(entry, "claim_recovery"));
            } else if is_operator_requeue(entry) {
                hands.push(human_recovery(entry, "operator_requeue"));
            }
        }
        hands
    }

    /// A final-recovery decision that brought the task back: completed it
    /// on a covering commit or requeued it. A triage requeue records the same
    /// decision; its marker is its hand.
    fn final_recovery_hand(&self, entry: &TaskHistoryEntry) -> Option<Hand> {
        if entry.event != FINAL_RECOVERY_DECIDED_EVENT {
            return None;
        }
        let Some(TaskDecision::FinalRecovery {
            run_id,
            decision,
            outcome,
        }) = &entry.decision
        else {
            return None;
        };
        if decision == TRIAGE_REQUEUE_DECISION
            || !matches!(
                outcome,
                RecordedFinalRecovery::Completed { .. } | RecordedFinalRecovery::Requeued
            )
        {
            return None;
        }
        let mut hand = Hand::new(RECOVERED, entry.at);
        hand.via = Some("final_recovery");
        hand.model = self.recovery_model(run_id, entry.at);
        hand.run_id = Some(run_id.clone());
        Some(hand)
    }

    /// The model of the run's latest recovery invocation at or before `at`.
    fn recovery_model(&self, run_id: &str, at: DateTime<Utc>) -> Option<String> {
        let records = self
            .workspace_invocation_records(InvocationQuery {
                job_run_id: Some(run_id.to_string()),
                until: Some(at),
                limit: RECOVERY_INVOCATION_LIMIT,
                ..InvocationQuery::default()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(run_id, %error, "handled by: recovery invocations unreadable");
                Vec::new()
            });
        records
            .into_iter()
            .filter(|record| record.activity_id.contains("recover"))
            .max_by_key(|record| record.ts)
            .and_then(|record| record.model)
    }

    fn readable_run(&self, run_id: &str) -> Option<orbit_types::workflow::JobRun> {
        self.get_job_run_backend(run_id).unwrap_or_else(|error| {
            tracing::warn!(run_id, %error, "handled by: job run unreadable");
            None
        })
    }
}

/// A triage marker whose requeue went through.
fn triage_requeued(entry: &TaskHistoryEntry) -> bool {
    entry.note.as_deref().is_some_and(|note| {
        note.split_whitespace()
            .any(|field| field == "outcome=requeued")
    })
}

/// A triage requeue: who asked for the triage, from the comment written
/// with the marker, and the blocking run its final-recovery decision names.
fn triage_hand(row: &TaskRow, entry: &TaskHistoryEntry) -> Hand {
    let mut hand = Hand::new(RECOVERED, entry.at);
    hand.via = Some("blocked_task_triage");
    hand.actor = row
        .comments
        .iter()
        .filter(|comment| comment.at == entry.at)
        .find_map(|comment| {
            let header = comment
                .message
                .strip_prefix(TRIAGE_COMMENT_LEAD)?
                .lines()
                .next()?;
            header
                .split_whitespace()
                .find_map(|field| field.strip_prefix("by="))
                .filter(|by| !by.is_empty())
                .map(str::to_string)
        });
    hand.run_id = row.history.iter().find_map(|other| match &other.decision {
        Some(TaskDecision::FinalRecovery {
            run_id, decision, ..
        }) if other.at == entry.at && decision == TRIAGE_REQUEUE_DECISION => Some(run_id.clone()),
        _ => None,
    });
    hand
}

/// A recovery a person or agent session made by hand: `operator` for a
/// human, otherwise the recorded actor label.
fn human_recovery(entry: &TaskHistoryEntry, via: &'static str) -> Hand {
    let mut hand = Hand::new(RECOVERED, entry.at);
    hand.via = Some(via);
    hand.actor = Some(match actor_kind(&entry.by) {
        "operator" => OPERATOR.to_string(),
        _ => entry.by.clone(),
    });
    hand
}
