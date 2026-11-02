//! Widening and narrowing a mandate's authority.
//!
//! Widening is the human's. A native operator caller (the bare CLI on this
//! host: operator capability, no MCP transport, no managed-run binding or
//! agent envelope, no agent identity) applies it at once with `via:
//! operator`. Any other caller, including an MCP session that holds operator
//! capability, writes nothing to the authority: its widening raises one
//! `resident_authority` decision request bound to the authority's revision,
//! quoting the relayed words verbatim. The request's `grant` answer, from a
//! human through the decision answer path, applies exactly that addition
//! with `via: decision:<id>`. Narrowing applies for any caller that can
//! write the assignment and records that caller.

use chrono::{DateTime, Utc};
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState, RESIDENT_AUTHORITY_GRANT,
    RESIDENT_AUTHORITY_REJECT, TaskArtifact,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{
    RESIDENT_MANDATE_ARTIFACT, RESIDENT_MAX_QUOTE_CHARS, RESIDENT_TAG, RESIDENT_VIA_NARROWING,
    RESIDENT_VIA_OPERATOR, ResidentAuthority, ResidentAuthorityChange, ResidentAuthorityEntry,
    ResidentGrantRequest, ResidentMandate, ResidentRelay, ResidentRight, canonical_rights,
};

use super::{resident_mandate_ended, resident_mandate_from_artifact};
use crate::OrbitRuntime;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};
use crate::runtime::authorization::resolved_caller_capabilities;
use crate::runtime::workspace::catalog::WorkspaceScope;

/// The `raised_by` mechanism of a resident grant request.
const RESIDENT_GRANT_MECHANISM: &str = "resident_grant";

/// What the tool layer knows about a grant caller. Whether it may widen
/// directly is decided here, never from input.
#[derive(Debug, Clone, Default)]
pub struct ResidentGrantCaller {
    /// The calling session; its capabilities, transport and worker binding
    /// decide whether the caller is the operator.
    pub session: ToolSessionContext,
    /// Agent identity the call carried.
    pub agent: Option<String>,
    /// Model the call carried.
    pub model: Option<String>,
    /// Orchestrator crew the session acts for.
    pub orchestrator: Option<String>,
}

impl ResidentGrantCaller {
    /// Whether this caller is the human operator on this host. An MCP
    /// session never is: a relay through a session is a request, whatever
    /// capability the session holds.
    fn is_native_operator(&self) -> bool {
        self.agent.is_none()
            && self.model.is_none()
            && self.orchestrator.is_none()
            && self.session.transport.is_none()
            && self.session.origin_session_id.is_none()
            && self.session.worker_invocation.is_none()
            && !agent_context_declared()
            && resolved_caller_capabilities(&self.session).contains(&McpCapability::Operator)
    }
}

/// One grant call's change.
#[derive(Debug, Clone, Default)]
pub struct ResidentGrantParams {
    /// The mandate's assignment task.
    pub task_id: String,
    /// Rights to add.
    pub add: Vec<ResidentRight>,
    /// Workspaces to add to the scope.
    pub workspaces: Vec<String>,
    /// Rights to remove.
    pub remove: Vec<ResidentRight>,
    /// Workspaces to drop from the scope.
    pub drop_workspaces: Vec<String>,
    /// A shorter mandate deadline.
    pub deadline: Option<DateTime<Utc>>,
    /// The human's words behind a widening, verbatim.
    pub quote: Option<String>,
}

/// What one grant call did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResidentGrantOutcome {
    /// `granted`, `requested`, `narrowed` or `unchanged`.
    pub outcome: &'static str,
    /// The authority record after the call.
    pub authority: ResidentAuthority,
    /// The grant request a non-operator widening raised or found open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<DecisionRequest>,
    /// Whether that request already existed, so nothing was written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deduplicated: bool,
}

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

fn names(rights: &[ResidentRight]) -> String {
    rights
        .iter()
        .map(|right| right.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn dedup_workspaces(workspaces: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for workspace in workspaces {
        let workspace = workspace.trim();
        if !workspace.is_empty() && !out.iter().any(|seen| seen == workspace) {
            out.push(workspace.to_string());
        }
    }
    out
}

fn read_authority(
    history: &[orbit_types::task::TaskHistoryEntry],
) -> Result<ResidentAuthority, OrbitError> {
    ResidentAuthority::from_history(history).map_err(|error| {
        OrbitError::Execution(format!(
            "the mandate's authority record is unreadable, so it cannot change: {error}"
        ))
    })
}

impl OrbitRuntime {
    /// `orbit.resident.grant`: widen (as the operator, or as a grant request
    /// for anyone else) or narrow a mandate's authority.
    pub fn grant_resident_authority(
        &self,
        caller: &ResidentGrantCaller,
        params: ResidentGrantParams,
    ) -> Result<ResidentGrantOutcome, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let add = canonical_rights(&params.add);
        let workspaces = dedup_workspaces(&params.workspaces);
        let remove = canonical_rights(&params.remove);
        let drop_workspaces = dedup_workspaces(&params.drop_workspaces);
        let widening = !add.is_empty() || !workspaces.is_empty();
        let narrowing =
            !remove.is_empty() || !drop_workspaces.is_empty() || params.deadline.is_some();
        if widening && narrowing {
            return Err(invalid(
                "a grant either widens (add, workspaces) or narrows (remove, drop_workspaces, deadline), not both; make two calls",
            ));
        }
        if !widening && !narrowing {
            return Err(invalid(
                "name a change: add, workspaces, remove, drop_workspaces or deadline",
            ));
        }
        let task_id = params.task_id.trim().to_string();
        let writer = self
            .actor()
            .resolve_write_label(caller.agent.as_deref(), caller.model.as_deref())?;
        let operator = caller.is_native_operator();
        let mut outcome = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&task_id, &mut || {
                let task = self.get_task(&task_id)?;
                if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
                    return Err(invalid(format!(
                        "task {task_id} is not a resident mandate: tag it `{RESIDENT_TAG}` and attach {RESIDENT_MANDATE_ARTIFACT}"
                    )));
                }
                let history = self.get_task_history(&task_id)?;
                let authority = read_authority(&history)?;
                outcome = Some(if widening {
                    if resident_mandate_ended(task.status) {
                        return Err(OrbitError::PolicyDenied(format!(
                            "task {task_id} is {}, so its mandate has ended and takes no grant",
                            task.status
                        )));
                    }
                    let mandate = resident_mandate_from_artifact(self, &task_id)?
                        .map_err(|reason| invalid(format!("the mandate is not usable: {reason}")))?;
                    self.ensure_grantable_workspaces(&mandate, &workspaces)?;
                    let add: Vec<ResidentRight> = add
                        .iter()
                        .copied()
                        .filter(|right| !authority.holds(*right))
                        .collect();
                    let workspaces: Vec<String> = workspaces
                        .iter()
                        .filter(|workspace| !authority.covers(workspace))
                        .cloned()
                        .collect();
                    if add.is_empty() && workspaces.is_empty() {
                        ResidentGrantOutcome {
                            outcome: "unchanged",
                            authority,
                            request: None,
                            deduplicated: false,
                        }
                    } else if operator {
                        let entry = ResidentAuthorityEntry {
                            revision: authority.revision + 1,
                            at: Utc::now(),
                            by: writer.clone(),
                            via: RESIDENT_VIA_OPERATOR.to_string(),
                            added: add,
                            workspaces_added: workspaces,
                            quote: params
                                .quote
                                .clone()
                                .filter(|quote| !quote.trim().is_empty()),
                            ..Default::default()
                        };
                        let authority = self.append_resident_authority_change(
                            &task_id, &authority, entry, None,
                        )?;
                        ResidentGrantOutcome {
                            outcome: "granted",
                            authority,
                            request: None,
                            deduplicated: false,
                        }
                    } else {
                        self.raise_resident_grant_request(
                            &task,
                            authority,
                            add,
                            workspaces,
                            params.quote.as_deref(),
                            caller,
                            &writer,
                        )?
                    }
                } else {
                    self.narrow_resident_authority(
                        &task_id,
                        authority,
                        &remove,
                        &drop_workspaces,
                        params.deadline,
                        &writer,
                    )?
                });
                Ok(())
            })?;
        outcome.ok_or_else(|| {
            OrbitError::Execution(format!("the grant on task {task_id} was not recorded"))
        })
    }

    /// Every workspace a widening adds must be one the mandate lists and one
    /// this host serves: the current workspace or one its catalog resolves.
    fn ensure_grantable_workspaces(
        &self,
        mandate: &ResidentMandate,
        workspaces: &[String],
    ) -> Result<(), OrbitError> {
        for workspace in workspaces {
            if !mandate.covers_workspace(workspace) {
                return Err(invalid(format!(
                    "workspace `{workspace}` is not in the mandate's workspaces; edit the mandate first, since the authority's scope stays inside them"
                )));
            }
            let current =
                workspace == &self.workspace_id()? || workspace == &self.workspace_label();
            let registered = current
                || self.workspace_catalog().is_some_and(|catalog| {
                    catalog
                        .resolve_scope(&WorkspaceScope::Selectors(vec![workspace.clone()]))
                        .is_ok_and(|targets| !targets.is_empty())
                });
            if !registered {
                return Err(invalid(format!(
                    "workspace `{workspace}` is not registered on this owner host, so a resident here cannot be granted it"
                )));
            }
        }
        Ok(())
    }

    /// Raise, or return the open, grant request for exactly this addition.
    #[allow(clippy::too_many_arguments)]
    fn raise_resident_grant_request(
        &self,
        task: &orbit_types::task::Task,
        authority: ResidentAuthority,
        add: Vec<ResidentRight>,
        workspaces: Vec<String>,
        quote: Option<&str>,
        caller: &ResidentGrantCaller,
        writer: &str,
    ) -> Result<ResidentGrantOutcome, OrbitError> {
        let quote = quote.unwrap_or_default();
        if quote.trim().is_empty() {
            return Err(OrbitError::PolicyDenied(
                "only the operator widens a resident's authority directly; to ask the human, pass `quote` with their words verbatim and a grant request is raised for them to answer".into(),
            ));
        }
        if quote.chars().count() > RESIDENT_MAX_QUOTE_CHARS {
            return Err(invalid(format!(
                "`quote` must be at most {RESIDENT_MAX_QUOTE_CHARS} characters"
            )));
        }
        let existing = self
            .get_task_decision_requests(&task.id)?
            .into_iter()
            .find(|request| {
                request.gate == DecisionGate::ResidentAuthority
                    && request.state == DecisionRequestState::Open
                    && request
                        .resident_grant
                        .as_ref()
                        .is_some_and(|grant| grant.asks_for(&add, &workspaces))
            });
        if let Some(existing) = existing {
            return Ok(ResidentGrantOutcome {
                outcome: "requested",
                authority,
                request: Some(existing),
                deduplicated: true,
            });
        }
        let scope = if workspaces.is_empty() {
            &authority.workspaces
        } else {
            &workspaces
        };
        let question = format!(
            "Grant {} in {} to the resident for {}?",
            if add.is_empty() {
                "no new rights".to_string()
            } else {
                names(&add)
            },
            if scope.is_empty() {
                "no workspace".to_string()
            } else {
                scope.join(", ")
            },
            task.id
        );
        let relay = ResidentRelay {
            by: writer.to_string(),
            crew: caller.orchestrator.clone(),
            model: caller.model.clone(),
            session_id: caller.session.origin_session_id.clone(),
        };
        let request = DecisionRequest::new(
            task.id.clone(),
            DecisionGate::ResidentAuthority,
            DecisionGate::resident_authority_answers(),
            DecisionBinding::ResidentAuthority {
                revision: authority.revision,
            },
            RESIDENT_GRANT_MECHANISM.to_string(),
            Utc::now(),
        )
        .with_summary(question, None)
        .with_topic_and_filer(None, writer.to_string())
        .with_category(&self.workspace_id()?)
        .with_resident_grant(ResidentGrantRequest {
            rights: add,
            workspaces,
            quote: quote.to_string(),
            relay,
        });
        self.stores().task_records().update(
            &task.id,
            TaskRecordUpdateParams {
                actor: writer.to_string(),
                append_history: vec![request.history_entry(writer)],
                ..Default::default()
            },
        )?;
        let stored = self
            .get_task_decision_requests(&task.id)?
            .into_iter()
            .find(|candidate| candidate.id == request.id)
            .unwrap_or(request);
        Ok(ResidentGrantOutcome {
            outcome: "requested",
            authority,
            request: Some(stored),
            deduplicated: false,
        })
    }

    fn narrow_resident_authority(
        &self,
        task_id: &str,
        authority: ResidentAuthority,
        remove: &[ResidentRight],
        drop_workspaces: &[String],
        deadline: Option<DateTime<Utc>>,
        writer: &str,
    ) -> Result<ResidentGrantOutcome, OrbitError> {
        let removed: Vec<ResidentRight> = remove
            .iter()
            .copied()
            .filter(|right| authority.holds(*right))
            .collect();
        let dropped: Vec<String> = drop_workspaces
            .iter()
            .filter(|workspace| authority.covers(workspace))
            .cloned()
            .collect();
        let mut mandate_write = None;
        if let Some(deadline) = deadline {
            let mut mandate = resident_mandate_from_artifact(self, task_id)?
                .map_err(|reason| invalid(format!("the mandate is not usable: {reason}")))?;
            if mandate.deadline.is_some_and(|current| deadline >= current) {
                return Err(invalid(
                    "`deadline` only shortens the mandate; a later deadline is an ordinary mandate edit of resident/mandate.json",
                ));
            }
            mandate.deadline = Some(deadline);
            let content = serde_json::to_vec_pretty(&mandate)
                .map_err(|error| OrbitError::Execution(format!("serialize mandate: {error}")))?;
            mandate_write = Some(TaskArtifact {
                path: RESIDENT_MANDATE_ARTIFACT.to_string(),
                content,
                media_type: "application/json".to_string(),
                created_by: None,
            });
        }
        if removed.is_empty() && dropped.is_empty() && mandate_write.is_none() {
            return Ok(ResidentGrantOutcome {
                outcome: "unchanged",
                authority,
                request: None,
                deduplicated: false,
            });
        }
        let entry = ResidentAuthorityEntry {
            revision: authority.revision + 1,
            at: Utc::now(),
            by: writer.to_string(),
            via: RESIDENT_VIA_NARROWING.to_string(),
            removed,
            workspaces_dropped: dropped,
            deadline,
            ..Default::default()
        };
        let authority =
            self.append_resident_authority_change(task_id, &authority, entry, mandate_write)?;
        Ok(ResidentGrantOutcome {
            outcome: "narrowed",
            authority,
            request: None,
            deduplicated: false,
        })
    }

    /// Append one authority change (and, for a deadline narrowing, the
    /// rewritten mandate) and return the record as it then reads. The caller
    /// holds the task's write lock.
    fn append_resident_authority_change(
        &self,
        task_id: &str,
        before: &ResidentAuthority,
        entry: ResidentAuthorityEntry,
        mandate_write: Option<TaskArtifact>,
    ) -> Result<ResidentAuthority, OrbitError> {
        let mut rights = before.rights.clone();
        rights.extend(entry.added.iter().copied());
        rights.retain(|right| !entry.removed.contains(right));
        let mut workspaces = before.workspaces.clone();
        for workspace in &entry.workspaces_added {
            if !workspaces.contains(workspace) {
                workspaces.push(workspace.clone());
            }
        }
        workspaces.retain(|workspace| !entry.workspaces_dropped.contains(workspace));
        let actor = entry.by.clone();
        let change = ResidentAuthorityChange {
            entry,
            rights: canonical_rights(&rights),
            workspaces,
        };
        self.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor,
                append_history: vec![change.history_entry()],
                upsert_artifacts: mandate_write.into_iter().collect(),
                ..Default::default()
            },
        )?;
        read_authority(&self.get_task_history(task_id)?)
    }

    /// The gate's write for an allowed answer to a `resident_authority`
    /// request, as the human `actor_label`. The caller holds the task's write
    /// lock and has refused stale and disallowed answers. `reject` changes
    /// nothing; `grant` adds exactly the requested rights and workspaces.
    pub(crate) fn apply_resident_grant_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        actor_label: &str,
    ) -> Result<(), OrbitError> {
        let not_applicable = |why: String| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!("{why}; nothing was applied and the request stays open"),
            )
        };
        match answer_id {
            RESIDENT_AUTHORITY_REJECT => return Ok(()),
            RESIDENT_AUTHORITY_GRANT => {}
            other => {
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::AnswerNotAllowed,
                    format!("`{other}` is not a grant answer; nothing was applied"),
                ));
            }
        }
        if is_automation_actor(actor_label) {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::CallerNotAllowed,
                format!("a resident grant is the human's; `{actor_label}` is automation"),
            ));
        }
        let grant = request.resident_grant.as_ref().ok_or_else(|| {
            not_applicable(format!(
                "decision request `{}` carries no requested addition",
                request.id
            ))
        })?;
        let task_id = request.task_id.as_str();
        let task = self.get_task(task_id)?;
        if !task.tags.iter().any(|tag| tag == RESIDENT_TAG) {
            return Err(not_applicable(format!(
                "task {task_id} is no longer tagged `{RESIDENT_TAG}`"
            )));
        }
        let history = self.get_task_history(task_id)?;
        let authority = read_authority(&history)?;
        let DecisionBinding::ResidentAuthority { revision } = request.binding else {
            return Err(not_applicable(format!(
                "decision request `{}` is not bound to an authority revision",
                request.id
            )));
        };
        if revision != authority.revision {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestSuperseded,
                format!(
                    "the authority changed since decision request `{}` was raised; nothing was applied",
                    request.id
                ),
            ));
        }
        let mandate = resident_mandate_from_artifact(self, task_id)?
            .map_err(|reason| not_applicable(format!("the mandate is not usable: {reason}")))?;
        self.ensure_grantable_workspaces(&mandate, &grant.workspaces)
            .map_err(|error| not_applicable(error.to_string()))?;
        let entry = ResidentAuthorityEntry {
            revision: authority.revision + 1,
            at: Utc::now(),
            by: actor_label.to_string(),
            via: format!("decision:{}", request.id),
            added: grant
                .rights
                .iter()
                .copied()
                .filter(|right| !authority.holds(*right))
                .collect(),
            workspaces_added: grant
                .workspaces
                .iter()
                .filter(|workspace| !authority.covers(workspace))
                .cloned()
                .collect(),
            quote: Some(grant.quote.clone()),
            relay: Some(grant.relay.clone()),
            ..Default::default()
        };
        // Everything asked for is already held: the answer closes the
        // request without an empty change.
        if !entry.is_widening() {
            return Ok(());
        }
        self.append_resident_authority_change(task_id, &authority, entry, None)?;
        Ok(())
    }
}
