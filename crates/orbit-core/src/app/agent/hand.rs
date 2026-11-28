//! The hands a task met beyond its stored attribution fields: who piloted,
//! reviewed and recovered it, in time order, for the task detail's
//! **Handled by** row.
//!
//! Every hand comes from a record Orbit wrote, never from an agent message:
//!
//! - **piloted**: each task-pilot receipt the apply boundary sealed, with the
//!   frozen activity crew draw and producing invocation model;
//! - **reviewed**: each typed or legacy review gate settlement, with the reviewer it
//!   names and its verdict, and the attempt's run from the lineage ledger of
//!   the task's latest gate certificate (older lineages carry no run);
//! - **recovered**: a final-recovery decision that completed or requeued the
//!   task (its model from the recovery invocation of that run), a
//!   blocked-task triage requeue, an operator's claim recovery to the
//!   backlog, and an operator requeue.
//!
//! Nothing here is stored on the task; the stored `created_by`,
//! `planned_by`, `implemented_by` and `orchestrator` fields stay the
//! dashboard's own, with their crews named by [`super::crew_name`]. Each
//! hand's `crew` is a bare crew name: the recorded crew without its effort,
//! else the one crew running the recorded model, else null.

use std::collections::HashMap;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contract::{InvocationQuery, TaskRow};
use orbit_types::identity::{crew_reference_base, infer_agent_family_from_model};
use orbit_types::task::{
    FINAL_RECOVERY_DECIDED_EVENT, RecordedFinalRecovery, TaskDecision, TaskHistoryEntry, TaskStatus,
};
use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate};
use serde_json::{Map, Value, json};

use super::crew_name::CrewNames;
use super::fact::{self, FindingState};
use super::record::is_operator_requeue;
use super::role::pilot_warnings;
use crate::OrbitRuntime;
use crate::Task;
use crate::app::codebase::{PilotRuns, actor_kind};
use crate::application::task::BLOCKED_TASK_TRIAGED_EVENT;
use crate::application::task::blocked_recovery::{
    self, LegacyBlockedTriageOutcome, LegacyBlockedTriageRecord,
};

/// A hand's verb on the task.
const PILOTED: &str = "piloted";

const REVIEWED: &str = "reviewed";

const RECOVERED: &str = "recovered";

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

/// The task's piloted, reviewed and recovered hands, oldest first, as
/// the task detail's `handled_by` list. A record whose run cannot be
/// read still yields its hand, without the crew or model that run would
/// have named.
pub fn task_handled_by(rt: &OrbitRuntime, row: &TaskRow) -> Vec<Value> {
    let mut hands = piloted_hands(rt, row);
    hands.extend(reviewed_hands(rt, row));
    hands.extend(recovered_hands(rt, row));
    hands.sort_by_key(|hand| hand.at);
    // The crew each hand shows: its recorded crew without the effort,
    // else the one crew running its recorded model, else none.
    let names = crew_names(rt);
    for hand in &mut hands {
        hand.crew = names.recorded(hand.crew.as_deref(), hand.model.as_deref());
    }
    hands.iter().map(Hand::to_json).collect()
}

/// The crew each stored attribution field of `task` names, as the task
/// detail's `recorded_crews`: `created_by`, `planned_by`,
/// `implemented_by` and `orchestrator`, each present only when the
/// stored record establishes a crew. The task's current `crew` is never
/// consulted, so a reassignment cannot rewrite who did earlier work.
pub(crate) fn task_recorded_crews(rt: &OrbitRuntime, task: &Task) -> Map<String, Value> {
    let names = crew_names(rt);
    let mut crews = Map::new();
    let mut put = |field: &str, crew: Option<String>| {
        if let Some(crew) = crew {
            crews.insert(field.to_string(), Value::String(crew));
        }
    };
    let label = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| names.by_label(value))
    };
    put(
        "created_by",
        label(&task.created_by_model).or_else(|| label(&task.created_by)),
    );
    put("planned_by", label(&task.planned_by));
    put("implemented_by", label(&task.implemented_by));
    // The orchestrator is a validated crew name when it is recorded.
    put(
        "orchestrator",
        task.orchestrator
            .as_deref()
            .map(crew_reference_base)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
    );
    crews
}

fn piloted_hands(rt: &OrbitRuntime, row: &TaskRow) -> Vec<Hand> {
    let comments = match rt.observe_task_pilot_comments(&row.task.id, &row.comments) {
        Ok(comments) => comments,
        Err(error) => {
            tracing::warn!(task_id = %row.task.id, %error, "pilot hands: assessment unreadable");
            return Vec::new();
        }
    };
    let assessments = pilot_warnings::settle(&row.task, &row.history, &comments).assessments;
    if assessments.is_empty() {
        return Vec::new();
    }
    let runs = PilotRuns::load_for_receipts(
        rt,
        assessments
            .iter()
            .map(|assessment| assessment.run_id.as_deref()),
    );
    let now = Utc::now();
    assessments
        .into_iter()
        .map(|assessment| {
            let mut hand = Hand::new(PILOTED, assessment.at);
            if let Some(run) = runs.attribute(
                rt,
                row.task.id.as_str(),
                assessment.run_id.as_deref(),
                assessment.at,
                &assessment.operation_id,
                now,
            ) {
                hand.crew = run.crew;
                hand.model = run.model;
            }
            hand.run_id = assessment.run_id;
            hand
        })
        .collect()
}

fn reviewed_hands(rt: &OrbitRuntime, row: &TaskRow) -> Vec<Hand> {
    let settlements = fact::review_settlement_facts(&row.history, &row.comments);
    if settlements.is_empty() {
        return Vec::new();
    }
    let runs = review_attempt_runs(rt, row);
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
fn review_attempt_runs(rt: &OrbitRuntime, row: &TaskRow) -> HashMap<String, String> {
    if !row
        .artifacts
        .iter()
        .any(|file| file.path == REVIEW_GATE_ARTIFACT)
    {
        return HashMap::new();
    }
    let read = || -> Result<HashMap<String, String>, OrbitError> {
        let Some(artifact) = rt.get_task_artifact(&row.task.id, REVIEW_GATE_ARTIFACT)? else {
            return Ok(HashMap::new());
        };
        let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
            return Ok(HashMap::new());
        };
        let ledger = rt.sqlite_store()?.read().and_then(|snapshot| {
            snapshot
                .review()
                .ledger(&rt.workspace_id()?, &certificate.lineage_key)
        })?;
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

fn recovered_hands(rt: &OrbitRuntime, row: &TaskRow) -> Vec<Hand> {
    let mut hands = Vec::new();
    for entry in &row.history {
        if let Some(hand) = final_recovery_hand(rt, entry) {
            hands.push(hand);
        } else if entry.event == BLOCKED_TASK_TRIAGED_EVENT
            && let Some(record) =
                blocked_recovery::decode_blocked_task_triaged(entry, &row.history, &row.comments)
            && record.outcome == LegacyBlockedTriageOutcome::Requeued
        {
            hands.push(recovered_hand_from_legacy_triage(record));
        } else if entry.event == CLAIM_REVOKED_EVENT && entry.to_status == Some(TaskStatus::Backlog)
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
fn final_recovery_hand(rt: &OrbitRuntime, entry: &TaskHistoryEntry) -> Option<Hand> {
    if entry.event != FINAL_RECOVERY_DECIDED_EVENT {
        return None;
    }
    let Some(TaskDecision::FinalRecovery {
        run_id, outcome, ..
    }) = &entry.decision
    else {
        return None;
    };
    if blocked_recovery::is_legacy_triage_requeue_binding(entry)
        || !matches!(
            outcome,
            RecordedFinalRecovery::Completed { .. } | RecordedFinalRecovery::Requeued
        )
    {
        return None;
    }
    let mut hand = Hand::new(RECOVERED, entry.at);
    hand.via = Some("final_recovery");
    hand.model = recovery_model(rt, run_id, entry.at);
    hand.run_id = Some(run_id.clone());
    Some(hand)
}

/// The model of the run's latest recovery invocation at or before `at`.
fn recovery_model(rt: &OrbitRuntime, run_id: &str, at: DateTime<Utc>) -> Option<String> {
    let records = rt
        .workspace_invocation_records(InvocationQuery {
            job_run_id: Some(run_id.to_string()),
            until: Some(at),
            limit: RECOVERY_INVOCATION_LIMIT,
            // Recovery reads activity, timestamp and model only.
            skip_tool_calls: true,
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

/// The configured crews that recorded attribution is matched against.
fn crew_names(rt: &OrbitRuntime) -> CrewNames {
    CrewNames::from_registry(&rt.configured_crew_registry_projection())
}

/// A triage requeue hand: the actor and run the shared marker record decoded.
fn recovered_hand_from_legacy_triage(record: LegacyBlockedTriageRecord) -> Hand {
    let mut hand = Hand::new(RECOVERED, record.at);
    hand.via = Some("blocked_task_triage");
    hand.actor = record.actor;
    hand.run_id = record.run_id;
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
