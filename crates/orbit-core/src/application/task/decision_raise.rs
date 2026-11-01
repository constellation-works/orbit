//! Agent-raised decision requests: a bounded multiple-choice question on a
//! human-crew task.
//!
//! An agent that needs a person to choose raises the question here instead of
//! filing a free-text human task. The request lives in the task's history like
//! every other decision request: no store, no table. Answering is the
//! operator's (`answer_decision_request_as_human`), which records the option,
//! completes the task and so unblocks its `blocked_by` dependents.
//!
//! Raising is bounded: an open agent-raised request is counted per workspace
//! and per filer, the same open question on a task is deduplicated to the
//! request that already holds it, and a task holds one open request.

use orbit_common::OrbitError;
use orbit_types::task::{
    AgentDecisionOptionInput, DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState,
    TaskStatus, agent_decision_answers, normalize_agent_decision_topic,
};

use super::TaskRecordUpdateParams;
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;

/// Most open agent-raised decision requests one workspace holds.
const MAX_OPEN_AGENT_DECISIONS_PER_WORKSPACE: usize = 20;
/// Most open agent-raised decision requests one filer holds.
const MAX_OPEN_AGENT_DECISIONS_PER_FILER: usize = 5;
/// Longest question, in characters.
const MAX_QUESTION_CHARS: usize = 600;
/// The `raised_by` mechanism name an agent-raised request carries.
const AGENT_REQUEST_MECHANISM: &str = "agent_request";

/// One agent's request to raise a decision on a human-crew task.
#[derive(Debug, Clone)]
pub struct AgentDecisionRaise {
    /// The human-crew task the decision is for.
    pub task_id: String,
    /// The decision needed, in one or two plain-language sentences.
    pub question: String,
    /// Two to four options, exactly one recommended.
    pub options: Vec<AgentDecisionOptionInput>,
    /// Optional structured topic or corpus rule id.
    pub topic: Option<String>,
    /// Writer label of the raising agent; the per-filer cap counts by it.
    pub filer: String,
}

/// The request an agent's raise resolved to.
#[derive(Debug, Clone)]
pub struct AgentDecisionRaised {
    /// The open request, newly raised or the one already holding the question.
    pub request: DecisionRequest,
    /// Whether the request already existed, so nothing was written.
    pub deduplicated: bool,
}

/// A question in its comparison form: case and spacing never make two
/// otherwise identical open questions distinct.
fn question_key(question: &str) -> String {
    question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl OrbitRuntime {
    /// Raise an agent's multiple-choice decision request on a human-crew
    /// task, or return the open request that already holds the same question.
    ///
    /// Refuses, before anything is written, a task that is not assigned to a
    /// human crew or is already closed, a task that already holds a different
    /// open request, malformed options, and a raise past the per-workspace or
    /// per-filer cap of open agent-raised requests.
    pub fn raise_agent_decision_request(
        &self,
        params: AgentDecisionRaise,
    ) -> Result<AgentDecisionRaised, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let invalid = |error: &dyn std::fmt::Display| OrbitError::InvalidInput(error.to_string());
        let question = params.question.trim().to_string();
        if question.is_empty() {
            return Err(OrbitError::InvalidInput(
                "`question` must not be empty".into(),
            ));
        }
        if question.chars().count() > MAX_QUESTION_CHARS {
            return Err(OrbitError::InvalidInput(format!(
                "`question` must be at most {MAX_QUESTION_CHARS} characters: name the decision, not its whole history"
            )));
        }
        let (answers, recommended) =
            agent_decision_answers(&params.options).map_err(|e| invalid(&e))?;
        let topic = params
            .topic
            .as_deref()
            .map(normalize_agent_decision_topic)
            .transpose()
            .map_err(|e| invalid(&e))?;
        let filer = params.filer.trim().to_string();
        if filer.is_empty() {
            return Err(OrbitError::InvalidInput(
                "a decision request needs an identified filer: pass `model` with your agent family"
                    .into(),
            ));
        }
        let task_id = params.task_id.trim().to_string();
        let key = question_key(&question);

        let mut outcome: Option<AgentDecisionRaised> = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&task_id, &mut || {
                let task = self.get_task(&task_id)?;
                if !self.is_human_crew(task.crew.as_deref()) {
                    return Err(OrbitError::PolicyDenied(format!(
                        "task {task_id} is not assigned to a human crew, so it cannot hold a decision request; assign it to a human crew or file a human-crew task for the decision"
                    )));
                }
                if matches!(
                    task.status,
                    TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
                ) {
                    return Err(OrbitError::InvalidInput(format!(
                        "task {task_id} is {}, so nobody can answer a decision on it",
                        task.status
                    )));
                }
                // The workspace scan reads every task, so the duplicate and
                // both caps are judged on the same snapshot.
                let open: Vec<DecisionRequest> = self
                    .list_decision_requests(Some(DecisionGate::AgentRequest), None, false)?
                    .into_iter()
                    .filter(|request| request.state == DecisionRequestState::Open)
                    .collect();
                if let Some(existing) = open.iter().find(|request| {
                    request.task_id == task_id
                        && request.topic == topic
                        && request.summary.as_deref().map(question_key).as_deref() == Some(&key)
                }) {
                    outcome = Some(AgentDecisionRaised {
                        request: existing.clone(),
                        deduplicated: true,
                    });
                    return Ok(());
                }
                if let Some(held) = open.iter().find(|request| request.task_id == task_id) {
                    return Err(OrbitError::InvalidInput(format!(
                        "task {task_id} already holds open decision request {}; wait for its answer before raising another",
                        held.id
                    )));
                }
                if open.len() >= MAX_OPEN_AGENT_DECISIONS_PER_WORKSPACE {
                    return Err(OrbitError::PolicyDenied(format!(
                        "this workspace already holds {} open agent-raised decision requests (cap {MAX_OPEN_AGENT_DECISIONS_PER_WORKSPACE}); nothing was raised. Wait for the operator to answer some, or fold the question into an existing request",
                        open.len()
                    )));
                }
                let by_filer = open
                    .iter()
                    .filter(|request| request.filer.as_deref() == Some(filer.as_str()))
                    .count();
                if by_filer >= MAX_OPEN_AGENT_DECISIONS_PER_FILER {
                    return Err(OrbitError::PolicyDenied(format!(
                        "`{filer}` already holds {by_filer} open agent-raised decision requests (cap {MAX_OPEN_AGENT_DECISIONS_PER_FILER}); nothing was raised. Wait for answers before raising more"
                    )));
                }
                let request = DecisionRequest::new(
                    task_id.clone(),
                    DecisionGate::AgentRequest,
                    answers.clone(),
                    DecisionBinding::for_task(&task, None),
                    AGENT_REQUEST_MECHANISM.to_string(),
                    chrono::Utc::now(),
                )
                .with_summary(question.clone(), Some(recommended.clone()))
                .with_topic_and_filer(topic.clone(), filer.clone());
                self.stores().task_records().update(
                    &task_id,
                    TaskRecordUpdateParams {
                        actor: filer.clone(),
                        append_history: vec![request.history_entry(&filer)],
                        ..Default::default()
                    },
                )?;
                let stored = self
                    .get_task_decision_requests(&task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request.id)
                    .unwrap_or(request);
                outcome = Some(AgentDecisionRaised {
                    request: stored,
                    deduplicated: false,
                });
                Ok(())
            })?;
        outcome.ok_or_else(|| {
            OrbitError::Execution(format!(
                "decision request on task {task_id} was not recorded"
            ))
        })
    }
}
