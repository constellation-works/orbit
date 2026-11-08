//! Who may call the crew memory tools, and on which key (slice M3, design
//! §6.3 and §9).
//!
//! An operator (operator capability, no run binding) keeps the M1 surface.
//! A call bound to a managed run is the run's: `orbit.memory.write` and
//! `orbit.memory.propose` are admitted only from a dreaming task's run, with
//! the key fixed to that task's `crew_memory_key` (another model's key and
//! `global` are refused), its citations held to its evidence pack and its
//! proposals capped at two. `orbit.memory.show` is admitted from a dreaming
//! run or a run in a memory arm, for its own model's space and `global`.
//! An unbound agent session is refused. The same rules decide which runs'
//! activities list the tools at all.

use std::collections::BTreeSet;

use chrono::{SubsecRound, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    CrewMemoryEventRow, CrewMemoryProposalOutcome, CrewMemoryProposalRow,
};
use orbit_types::task::Task;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::dream::{CREW_MEMORY_AGENT_TOOLS, DREAM_MINTED, dreaming_task_key};
use super::operations::{DreamingWrite, WriteAction, json_text};
use super::{
    MemoryFile, MemoryKey, MemoryWrite, MemoryWriteContext, MemoryWriter, memory_key,
    validate_memory_write,
};
use crate::OrbitRuntime;

/// Proposals one dreaming run may open.
pub const CREW_MEMORY_MAX_PROPOSALS_PER_RUN: u32 = 2;
/// The activity whose dreaming run may write and propose.
const DREAMING_ACTIVITY: &str = "agent_implement";
const SHOW_TOOL: &str = "orbit.memory.show";

/// What the tool layer knows about a crew memory tool caller. Run and task
/// ids come from the trusted run binding, never from tool input.
#[derive(Debug, Clone, Default)]
pub struct MemoryToolCaller {
    /// Operator capability with no worker binding.
    pub operator: bool,
    /// Agent capability.
    pub agent: bool,
    /// A claimed leaf's worker binding: its task and bound run.
    pub worker_task_id: Option<String>,
    pub worker_run_id: Option<String>,
    /// A managed run's binding on this host.
    pub scope_task_id: Option<String>,
    pub scope_run_id: Option<String>,
    /// The calling run's model, when it named one.
    pub model: Option<String>,
}

enum ResolvedCaller {
    Operator,
    Run {
        task: Box<Task>,
        run_id: String,
        claimed: bool,
    },
    Unbound,
}

/// A dreaming run's admitted scope.
struct DreamingScope {
    key: MemoryKey,
    evidence: BTreeSet<String>,
}

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

impl OrbitRuntime {
    fn resolve_memory_caller(
        &self,
        caller: &MemoryToolCaller,
    ) -> Result<ResolvedCaller, OrbitError> {
        let non_empty = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        if let (Some(task_id), Some(run_id)) = (
            non_empty(&caller.worker_task_id),
            non_empty(&caller.worker_run_id),
        ) {
            return Ok(ResolvedCaller::Run {
                task: Box::new(self.get_task(&task_id)?),
                run_id,
                claimed: true,
            });
        }
        if let Some(run_id) = non_empty(&caller.scope_run_id) {
            let task = match non_empty(&caller.scope_task_id) {
                Some(task_id) => Some(self.get_task(&task_id)?),
                None => {
                    let tasks = self.list_run_tasks(&run_id)?;
                    let dreaming = tasks
                        .iter()
                        .position(|task| dreaming_task_key(task).is_some());
                    match dreaming {
                        Some(index) => tasks.into_iter().nth(index),
                        None => tasks.into_iter().next(),
                    }
                }
            };
            if let Some(task) = task {
                return Ok(ResolvedCaller::Run {
                    task: Box::new(task),
                    run_id,
                    claimed: false,
                });
            }
        }
        if caller.operator {
            return Ok(ResolvedCaller::Operator);
        }
        Ok(ResolvedCaller::Unbound)
    }

    /// The scope of `task`'s dreaming run, or why `verb` is refused there.
    fn dreaming_scope(
        &self,
        task: &Task,
        claimed: bool,
        verb: &str,
    ) -> Result<DreamingScope, OrbitError> {
        let Some(key) = dreaming_task_key(task) else {
            return Err(refused(format!(
                "crew memory {verb} refused: only a crew memory dreaming task's run may {verb}, and task {} is not one",
                task.id
            )));
        };
        if claimed {
            return Err(refused(format!(
                "crew memory {verb} refused: crew memory lives in the owner's store, so dreaming task {} runs only on the owner",
                task.id
            )));
        }
        if task.status.is_terminal() {
            return Err(refused(format!(
                "crew memory {verb} refused: dreaming task {} is {}",
                task.id, task.status
            )));
        }
        let events = self
            .crew_memory_store()?
            .crew_memory_events(&self.workspace_id()?, key.as_str())?;
        let evidence = events
            .iter()
            .rev()
            .filter(|event| event.action == DREAM_MINTED)
            .filter_map(|event| serde_json::from_str::<Value>(&event.detail_json).ok())
            .find(|detail| detail["task_id"].as_str() == Some(task.id.as_str()))
            .map(|detail| {
                detail["evidence_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>()
            })
            .ok_or_else(|| {
                refused(format!(
                    "crew memory {verb} refused: no evidence pack was recorded for dreaming task {}",
                    task.id
                ))
            })?;
        Ok(DreamingScope { key, evidence })
    }

    /// Record a dreaming run's refused call on its own key's audit.
    fn record_scope_refusal(
        &self,
        key: &MemoryKey,
        action: &str,
        task_id: &str,
        rule: &str,
        message: &str,
    ) -> Result<(), OrbitError> {
        self.crew_memory_store()?
            .append_crew_memory_event(&CrewMemoryEventRow {
                workspace_id: self.workspace_id()?,
                memory_key: key.to_string(),
                at: Utc::now().trunc_subsecs(6),
                action: action.to_string(),
                actor: "run".to_string(),
                revision: None,
                detail_json: json!({ "task_id": task_id, "rule": rule, "message": message })
                    .to_string(),
            })
    }

    /// `orbit.memory.show` for `caller`.
    pub fn crew_memory_tool_show(
        &self,
        caller: &MemoryToolCaller,
        key: Option<&str>,
        revision: Option<u32>,
    ) -> Result<Value, OrbitError> {
        let (task, claimed) = match self.resolve_memory_caller(caller)? {
            ResolvedCaller::Operator => return self.crew_memory_show(key, revision),
            ResolvedCaller::Unbound if !caller.agent => {
                return self.crew_memory_show(key, revision);
            }
            ResolvedCaller::Unbound => {
                return Err(refused(
                    "crew memory show refused: an agent reads crew memory only from a crew memory dreaming task's run or a run in a memory experiment arm",
                ));
            }
            ResolvedCaller::Run { task, claimed, .. } => (task, claimed),
        };
        if claimed {
            return Err(refused(
                "crew memory show refused: crew memory lives in the owner's store; a claimed run receives its memory in its envelope",
            ));
        }
        let own = match dreaming_task_key(&task) {
            Some(key) => key,
            None if self.task_in_memory_arm(&task.id)? => {
                memory_key(caller.model.as_deref()).ok_or_else(|| {
                    refused(
                        "crew memory show refused: this run records no model, and a run with no recorded model has no memory",
                    )
                })?
            }
            None => {
                return Err(refused(format!(
                    "crew memory show refused: task {} is neither a crew memory dreaming task nor in a memory experiment arm",
                    task.id
                )));
            }
        };
        let requested = match key.map(str::trim).filter(|key| !key.is_empty()) {
            None => own.clone(),
            Some(raw) => super::parse_memory_key(raw)?,
        };
        if !requested.is_global() && requested != own {
            return Err(refused(format!(
                "crew memory show refused: this run may read only `{own}` and `global`, not `{requested}`"
            )));
        }
        self.crew_memory_show(Some(requested.as_str()), revision)
    }

    /// `orbit.memory.write` for `caller`: the operator's write, or a
    /// dreaming run's write of its own key.
    pub fn crew_memory_tool_write(
        &self,
        caller: &MemoryToolCaller,
        key: &str,
        write: MemoryWrite,
        reason: Option<String>,
    ) -> Result<Value, OrbitError> {
        let (task, run_id, claimed) = match self.resolve_memory_caller(caller)? {
            ResolvedCaller::Operator => return self.crew_memory_write(key, write, reason),
            ResolvedCaller::Unbound => {
                return Err(refused(
                    "crew memory write refused: only an operator, or a crew memory dreaming task's run writing its own model's space, may write crew memory",
                ));
            }
            ResolvedCaller::Run {
                task,
                run_id,
                claimed,
            } => (task, run_id, claimed),
        };
        let scope = self.dreaming_scope(&task, claimed, "write")?;
        let requested = super::parse_memory_key(key)?;
        if requested != scope.key {
            let message = if requested.is_global() {
                format!(
                    "crew memory write refused: a dreaming run writes only its own space `{}`; propose a global note with orbit.memory.propose instead",
                    scope.key
                )
            } else {
                format!(
                    "crew memory write refused: dreaming task {} writes only `{}`, not `{requested}`",
                    task.id, scope.key
                )
            };
            self.record_scope_refusal(
                &scope.key,
                "write_refused",
                &task.id,
                "key_scope",
                &message,
            )?;
            return Err(refused(message));
        }
        let writer = MemoryWriter::Run {
            run_id,
            crew: task.crew.clone().unwrap_or_default(),
            actor: task.crew.clone().unwrap_or_else(|| scope.key.to_string()),
        };
        self.write_crew_memory_revision(
            &scope.key,
            write,
            &writer,
            WriteAction::Write,
            reason,
            Some(&DreamingWrite {
                task_id: &task.id,
                evidence_pack: &scope.evidence,
            }),
        )
    }

    /// `orbit.memory.propose` for `caller`: a dreaming run's proposal for
    /// the global memory, at most [`CREW_MEMORY_MAX_PROPOSALS_PER_RUN`] per
    /// run.
    pub fn crew_memory_tool_propose(
        &self,
        caller: &MemoryToolCaller,
        text: String,
        evidence: Vec<String>,
    ) -> Result<Value, OrbitError> {
        let (task, run_id, claimed) = match self.resolve_memory_caller(caller)? {
            ResolvedCaller::Run {
                task,
                run_id,
                claimed,
            } => (task, run_id, claimed),
            ResolvedCaller::Operator | ResolvedCaller::Unbound => {
                return Err(refused(
                    "crew memory propose refused: only a crew memory dreaming task's run may propose a global note; an operator writes the global memory with `orbit memory write global`",
                ));
            }
        };
        let scope = self.dreaming_scope(&task, claimed, "propose")?;
        let global = MemoryKey::global();
        self.ensure_crew_memory_writes_enabled("propose", &global)?;
        self.ensure_coordination_task_write_permitted()?;
        let writer = MemoryWriter::Run {
            run_id: run_id.clone(),
            crew: task.crew.clone().unwrap_or_default(),
            actor: task.crew.clone().unwrap_or_else(|| scope.key.to_string()),
        };
        let write = MemoryWrite {
            files: vec![MemoryFile {
                path: "proposal.md".to_string(),
                text: text.clone(),
            }],
            evidence: evidence.clone(),
        };
        // A global note reaches every model, so every crew and model name is
        // another's.
        let vocabulary = self.crew_memory_vocabulary(&[]);
        let context = MemoryWriteContext {
            key: &global,
            writer: &writer,
            vocabulary: &vocabulary,
            evidence_pack: Some(&scope.evidence),
        };
        if let Err(refusal) = validate_memory_write(&write, &context) {
            let message = refusal.to_string().replacen("write", "proposal", 1);
            self.record_scope_refusal(
                &scope.key,
                "propose_refused",
                &task.id,
                refusal.rule.as_str(),
                &message,
            )?;
            return Err(OrbitError::InvalidInput(message));
        }
        let now = Utc::now().trunc_subsecs(6);
        let workspace_id = self.workspace_id()?;
        let id = proposal_id(&workspace_id, &scope.key, &run_id, &text, now);
        let outcome = self.crew_memory_store()?.open_crew_memory_proposal(
            &CrewMemoryProposalRow {
                workspace_id: workspace_id.clone(),
                id: id.clone(),
                proposing_key: scope.key.to_string(),
                proposing_run_id: Some(run_id.clone()),
                text,
                evidence_json: json_text(&evidence)?,
                status: "open".to_string(),
                created_at: now,
            },
            &CrewMemoryEventRow {
                workspace_id,
                memory_key: scope.key.to_string(),
                at: now,
                action: "proposal_opened".to_string(),
                actor: writer.actor().to_string(),
                revision: None,
                detail_json: json!({
                    "task_id": task.id,
                    "run_id": run_id,
                    "proposal_id": id,
                    "evidence": evidence,
                })
                .to_string(),
            },
            CREW_MEMORY_MAX_PROPOSALS_PER_RUN,
        )?;
        if let CrewMemoryProposalOutcome::CapReached { existing } = outcome {
            let message = format!(
                "crew memory propose refused: this run already opened {existing} proposal(s), the most one dreaming run may open is {CREW_MEMORY_MAX_PROPOSALS_PER_RUN}"
            );
            self.record_scope_refusal(
                &scope.key,
                "propose_refused",
                &task.id,
                "proposal_cap",
                &message,
            )?;
            return Err(refused(message));
        }
        Ok(json!({
            "id": id,
            "key": scope.key,
            "status": "open",
            "evidence": evidence,
            "task_id": task.id,
            "run_id": run_id,
        }))
    }

    /// The crew memory agent tools a run serving `tasks` in `activity` must
    /// not see. Write and propose belong to a dreaming task's implementer
    /// alone; show also to any run whose task is in a memory arm. `activity`
    /// is `None` where the caller cannot name it, which admits only show.
    pub(crate) fn crew_memory_tool_suppressions(
        &self,
        tasks: &[Task],
        activity: Option<&str>,
    ) -> Result<Vec<String>, OrbitError> {
        let dreaming = !tasks.is_empty()
            && tasks.iter().all(|task| dreaming_task_key(task).is_some())
            && activity == Some(DREAMING_ACTIVITY);
        let mut memory_arm = false;
        if !dreaming {
            for task in tasks {
                if self.task_in_memory_arm(&task.id)? {
                    memory_arm = true;
                    break;
                }
            }
        }
        Ok(CREW_MEMORY_AGENT_TOOLS
            .iter()
            .filter(|tool| !(dreaming || (memory_arm && **tool == SHOW_TOOL)))
            .map(|tool| (*tool).to_string())
            .collect())
    }
}

/// `cmp-<12 hex>` over the proposal's identity.
fn proposal_id(
    workspace_id: &str,
    key: &MemoryKey,
    run_id: &str,
    text: &str,
    at: chrono::DateTime<Utc>,
) -> String {
    let digest = Sha256::digest(
        format!(
            "{workspace_id}\n{key}\n{run_id}\n{}\n{text}",
            at.to_rfc3339()
        )
        .as_bytes(),
    );
    let hex: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("cmp-{hex}")
}
