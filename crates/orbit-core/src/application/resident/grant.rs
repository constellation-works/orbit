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
//!
//! The dashboard's Residents form is one operation of its own,
//! [`OrbitRuntime::grant_resident_authority_as_dashboard_operator`]: a
//! dashboard session is never the native operator, so its widening raises
//! the request and the operator answers it `grant` in the same action. Its
//! form may widen and narrow at once; that runs as two recorded changes,
//! widening first, and the outcome names both steps.

use chrono::{DateTime, Utc};
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_common::governance::resident::ResidentVerb;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState, RESIDENT_AUTHORITY_GRANT,
    RESIDENT_AUTHORITY_REJECT, TaskArtifact, TaskHistoryEntry, TaskStatus,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{
    RESIDENT_MANDATE_ARTIFACT, RESIDENT_MAX_QUOTE_CHARS, RESIDENT_TAG, RESIDENT_VIA_NARROWING,
    RESIDENT_VIA_OPERATOR, ResidentAuthority, ResidentAuthorityChange, ResidentAuthorityEntry,
    ResidentGrantRequest, ResidentMandate, ResidentRelay, ResidentRight, canonical_rights,
};

use super::control::{ResidentControlState, resident_control_from_history};
use super::{ResidentEffectiveAuthority, resident_mandate_ended, resident_mandate_from_artifact};
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

/// The dashboard Residents form's grant, as the operator sent it. The lists
/// are read exactly as `orbit.resident.grant` reads its input.
#[derive(Debug, Clone, Default)]
pub struct ResidentDashboardGrantParams {
    /// The mandate's assignment task.
    pub task_id: String,
    /// Rights to add.
    pub add: Vec<String>,
    /// Workspaces to add to the scope.
    pub workspaces: Vec<String>,
    /// Rights to remove.
    pub remove: Vec<String>,
    /// Workspaces to drop from the scope.
    pub drop_workspaces: Vec<String>,
    /// A shorter mandate deadline, RFC 3339.
    pub deadline: Option<String>,
    /// The operator's words behind a widening, recorded verbatim.
    pub quote: Option<String>,
    /// The human the widening's grant request is answered as.
    pub answered_by: String,
}

impl ResidentDashboardGrantParams {
    /// Whether the form asks for a widening (rights or workspaces to add).
    pub fn widens(&self) -> bool {
        !self.add.is_empty() || !self.workspaces.is_empty()
    }

    fn narrows(&self) -> bool {
        !self.remove.is_empty()
            || !self.drop_workspaces.is_empty()
            || non_blank(self.deadline.as_deref()).is_some()
    }
}

/// What a dashboard grant did, naming both steps of a mixed form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentDashboardGrantOutcome {
    /// The widening applied; any narrowing changed nothing.
    Granted,
    /// Nothing changed: every requested right and workspace was already
    /// held, or the narrowing removed nothing held.
    Unchanged,
    /// A narrowing-only form changed the authority.
    Narrowed,
    /// The widening applied, then the narrowing did.
    GrantedAndNarrowed,
    /// Every requested right and workspace was already held; the narrowing
    /// applied.
    UnchangedAndNarrowed,
    /// The widening applied and stays recorded; the narrowing was refused.
    GrantedNarrowingRefused,
    /// Every requested right and workspace was already held; the narrowing
    /// was refused.
    UnchangedNarrowingRefused,
}

/// The dashboard grant's result: the fields `POST /api/residents/<id>/grant`
/// renders, in that order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResidentDashboardGrant {
    /// What the grant did.
    pub outcome: ResidentDashboardGrantOutcome,
    /// Why the narrowing step was refused, after an applied or unchanged
    /// widening.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrowing_error: Option<String>,
    /// The answered grant request. Absent for a single unchanged or
    /// narrowing change; `null` for a mixed form whose widening was already
    /// held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<Option<DecisionRequest>>,
    /// The authority record after the grant.
    pub authority: Option<ResidentAuthority>,
    /// The effective authority after the grant, as `orbit.resident.show`
    /// reports it; absent for a single unchanged or narrowing change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_authority: Option<ResidentEffectiveAuthority>,
}

impl ResidentDashboardGrant {
    /// One core change, reported as `orbit.resident.grant` reports it.
    fn single_change(outcome: ResidentDashboardGrantOutcome, authority: ResidentAuthority) -> Self {
        Self {
            outcome,
            narrowing_error: None,
            request: None,
            authority: Some(authority),
            effective_authority: None,
        }
    }
}

/// The note and provenance a dashboard widening's grant answer records.
const RESIDENT_DASHBOARD_GRANT_NOTE: &str = "Granted from the dashboard Residents view";
const RESIDENT_DASHBOARD_PROVENANCE: &str = "dashboard";

fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn resident_rights_input(
    input: &OperationInput,
    field: &str,
) -> Result<Vec<ResidentRight>, OrbitError> {
    input
        .optional_string_list(field)?
        .unwrap_or_default()
        .iter()
        .map(|name| {
            ResidentRight::parse(name).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "unknown right `{name}` in `{field}`; rights are {}",
                    ResidentRight::NAMES
                ))
            })
        })
        .collect()
}

/// `orbit.resident.grant`'s parsed input as one grant call's change.
pub(crate) fn resident_grant_params_from_input(
    input: &OperationInput,
) -> Result<ResidentGrantParams, OrbitError> {
    let deadline = input
        .optional_string("deadline")?
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|at| at.with_timezone(&Utc))
                .map_err(|_| {
                    OrbitError::InvalidInput("`deadline` must be an RFC 3339 timestamp".into())
                })
        })
        .transpose()?;
    Ok(ResidentGrantParams {
        task_id: input.required_string("task_id")?,
        add: resident_rights_input(input, "add")?,
        workspaces: input
            .optional_string_list("workspaces")?
            .unwrap_or_default(),
        remove: resident_rights_input(input, "remove")?,
        drop_workspaces: input
            .optional_string_list("drop_workspaces")?
            .unwrap_or_default(),
        deadline,
        quote: input.optional_raw_string("quote")?,
    })
}

/// One half of a dashboard form, read as `orbit.resident.grant` reads its
/// input: only the non-empty lists and the given deadline and quote.
fn dashboard_grant_half(
    task_id: &str,
    lists: [(&str, &[String]); 2],
    deadline: Option<&str>,
    quote: Option<&str>,
) -> Result<ResidentGrantParams, OrbitError> {
    let mut input = serde_json::Map::new();
    input.insert("task_id".into(), serde_json::json!(task_id));
    for (key, list) in lists {
        if !list.is_empty() {
            input.insert(key.into(), serde_json::json!(list));
        }
    }
    if let Some(deadline) = deadline {
        input.insert("deadline".into(), serde_json::json!(deadline));
    }
    if let Some(quote) = quote {
        input.insert("quote".into(), serde_json::json!(quote));
    }
    let input = ResidentVerb::Grant
        .spec()
        .parse_input(serde_json::Value::Object(input))?;
    resident_grant_params_from_input(&input)
}

/// The dashboard's caller: operator capability without the native
/// terminal. A widening carries a relaying session so core raises the
/// request the operator then answers.
fn dashboard_grant_caller(relaying: bool) -> ResidentGrantCaller {
    let mut session = ToolSessionContext::default();
    session
        .effective_capabilities
        .extend([McpCapability::Agent, McpCapability::Operator]);
    if relaying {
        session.origin_session_id = Some(format!("dashboard:{}", audit_execution_id("resident")));
    }
    ResidentGrantCaller {
        session,
        ..Default::default()
    }
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

fn grant_mandate_ended(
    status: TaskStatus,
    history: &[TaskHistoryEntry],
) -> Result<bool, OrbitError> {
    Ok(resident_mandate_ended(status)
        || resident_control_from_history(history)
            .map_err(OrbitError::Execution)?
            .state
            == ResidentControlState::Ended)
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
                    if grant_mandate_ended(task.status, &history)? {
                        return Err(OrbitError::PolicyDenied(format!(
                            "task {task_id} has an ended mandate and takes no grant"
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

    /// The dashboard Residents form's grant, from the operator session.
    ///
    /// A narrowing-only form is one grant call. A widening needs the
    /// operator's `quote`; it raises the `resident_authority` request and
    /// answers it `grant` as `answered_by`, so the change is recorded as
    /// `via: decision:<id>`. A form that also narrows then narrows as a
    /// second recorded change; a refused narrowing leaves the widening
    /// recorded and is reported in `narrowing_error`, never as a refusal of
    /// the whole call. `orbit.resident.grant` itself still refuses one call
    /// that both widens and narrows.
    pub fn grant_resident_authority_as_dashboard_operator(
        &self,
        params: ResidentDashboardGrantParams,
    ) -> Result<ResidentDashboardGrant, OrbitError> {
        use ResidentDashboardGrantOutcome as Outcome;
        let deadline = non_blank(params.deadline.as_deref());
        let quote = non_blank(params.quote.as_deref());
        let narrow = || {
            dashboard_grant_half(
                &params.task_id,
                [
                    ("remove", &params.remove),
                    ("drop_workspaces", &params.drop_workspaces),
                ],
                deadline.as_deref(),
                None,
            )
            .and_then(|change| {
                self.grant_resident_authority(&dashboard_grant_caller(false), change)
            })
        };
        if !params.widens() {
            let narrowed = narrow()?;
            let outcome = if narrowed.outcome == "unchanged" {
                Outcome::Unchanged
            } else {
                Outcome::Narrowed
            };
            return Ok(ResidentDashboardGrant::single_change(
                outcome,
                narrowed.authority,
            ));
        }
        if quote.is_none() {
            return Err(invalid(
                "a widening needs your words authorizing it; they are recorded verbatim on the grant request",
            ));
        }
        let widening = dashboard_grant_half(
            &params.task_id,
            [("add", &params.add), ("workspaces", &params.workspaces)],
            None,
            quote.as_deref(),
        )?;
        let raised = self.grant_resident_authority(&dashboard_grant_caller(true), widening)?;
        // `unchanged` means every requested right and workspace was already
        // held: nothing widened and no request exists.
        let widening_applied = raised.outcome != "unchanged";
        if !widening_applied && !params.narrows() {
            return Ok(ResidentDashboardGrant::single_change(
                Outcome::Unchanged,
                raised.authority,
            ));
        }
        let request = match raised.request {
            Some(request) if raised.outcome == "requested" => {
                Some(self.answer_decision_request_as_human(
                    &request.id,
                    RESIDENT_AUTHORITY_GRANT,
                    Some(RESIDENT_DASHBOARD_GRANT_NOTE.to_string()),
                    params.answered_by.clone(),
                    Some(RESIDENT_DASHBOARD_PROVENANCE.to_string()),
                )?)
            }
            request => request,
        };
        // The widening is already recorded; a refused narrowing must not
        // report the whole call as refused.
        let (outcome, narrowing_error) = if !params.narrows() {
            (Outcome::Granted, None)
        } else {
            match (widening_applied, narrow()) {
                (true, Ok(narrowed)) if narrowed.outcome == "unchanged" => (Outcome::Granted, None),
                (false, Ok(narrowed)) if narrowed.outcome == "unchanged" => {
                    (Outcome::Unchanged, None)
                }
                (true, Ok(_)) => (Outcome::GrantedAndNarrowed, None),
                (false, Ok(_)) => (Outcome::UnchangedAndNarrowed, None),
                (true, Err(error)) => (Outcome::GrantedNarrowingRefused, Some(error.to_string())),
                (false, Err(error)) => {
                    (Outcome::UnchangedNarrowingRefused, Some(error.to_string()))
                }
            }
        };
        let task = self.get_task(params.task_id.trim())?;
        let history = self.get_task_history(&task.id)?;
        let view = self.resident_view(task, &history)?;
        Ok(ResidentDashboardGrant {
            outcome,
            narrowing_error,
            request: Some(request),
            effective_authority: Some(view.effective()),
            authority: view.authority.ok(),
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
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument feeds a different field of the stored grant request and its history: rights, workspaces, the verbatim quote, the relay caller and the history writer"
    )]
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
        if grant_mandate_ended(task.status, &history)? {
            return Err(not_applicable(format!(
                "task {task_id} has an ended mandate"
            )));
        }
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
