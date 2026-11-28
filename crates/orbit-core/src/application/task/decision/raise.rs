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

/// A deterministic interleaving at the raise path: a hook runs at the
/// raise lock boundary, so a test can synchronize concurrent raises.
#[cfg(feature = "test-support")]
pub(crate) mod raise_checkpoint {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    /// Take the hook before running it, so a hook that raises again does
    /// not run itself a second time.
    pub(super) fn notify() {
        let hook = HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Run `operation` with `hook` called once, on this thread, at the
    /// decision raise lock boundary.
    pub fn with_decision_raise_checkpoint<T>(
        hook: impl FnOnce() + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                HOOK.with(|slot| *slot.borrow_mut() = None);
            }
        }
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        let _clear = Clear;
        operation()
    }
}

use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::{DecisionRefusalCode as Refusal, OrbitError};
use orbit_tools::plugin::{
    CallbackResolution, PluginCallbackIdentity, resolve_plugin_callback_session,
};
use orbit_types::plugin::{
    InstalledPlugin, PluginExecutionKind, PluginMcpScope, namespace_collides_with_tool,
};
use orbit_types::task::{
    AgentDecisionOptionInput, DECISION_SUPERSEDED_EVENT, DECISION_WITHDRAWN_EVENT, DecisionBinding,
    DecisionGate, DecisionRequest, DecisionRequestState, PluginApply, Task, TaskHistoryEntry,
    TaskStatus, agent_decision_answers, decision_requests_from_history,
    normalize_agent_decision_topic,
};
use serde_json::{Value, json};

use super::plugin_raise::{
    MAX_INPUT_BYTES, Raise, RequestInput, check_caps, check_opaque, holder_title,
    is_plugin_decision_holder, matching_request, raised_json, refuse, validated_answers,
};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskRecordUpdateParams, TaskRecords};

/// Most open agent-raised decision requests one workspace holds.
const MAX_OPEN_AGENT_DECISIONS_PER_WORKSPACE: usize = 20;
/// Most open agent-raised decision requests one filer holds.
const MAX_OPEN_AGENT_DECISIONS_PER_FILER: usize = 5;
/// Longest question, in characters.
pub(super) const MAX_QUESTION_CHARS: usize = 600;
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

        #[cfg(feature = "test-support")]
        raise_checkpoint::notify();
        // Lock order for the whole raise: the workspace raise lock first,
        // then the target task's write lock. The workspace scan runs under
        // the raise lock but before the task lock, so it never waits on
        // another task's bundle while holding this task's bundle. A raise
        // for another task waits on the raise lock while holding nothing,
        // so the two raises cannot form the bundle-lock cycle the scan
        // under the target lock allowed. Only raises take this lock, and
        // only raises create open agent requests, so the scanned cap
        // snapshot cannot gain a request from a concurrent raise.
        // The shared Orbit root is read-only in managed lanes. Keep this
        // workspace-wide lock inside the already-granted task store so a
        // permitted raise needs no broader filesystem authority.
        let raise_lock_target = self.paths().orbit_dir.join("tasks/decision-raise");
        let mut outcome: Option<AgentDecisionRaised> = None;
        orbit_common::fs::io::with_exclusive_file_lock(
            &raise_lock_target,
            "decision raise",
            || -> Result<(), OrbitError> {
                let check_target = |task: &orbit_types::task::Task| -> Result<(), OrbitError> {
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
                    Ok(())
                };
                check_target(&self.get_task(&task_id)?)?;
                // The workspace scan reads every task, so both caps are
                // judged on the same snapshot. It runs before the target
                // task lock is taken (see the lock order above).
                let open: Vec<DecisionRequest> = self
                    .list_decision_requests(Some(DecisionGate::AgentRequest), None, false)?
                    .into_iter()
                    .filter(|request| request.state == DecisionRequestState::Open)
                    .collect();
                self.stores()
                    .tasks()
                    .with_task_write_lock(&task_id, &mut || {
                        let task = self.get_task(&task_id)?;
                        check_target(&task)?;
                        // Only this task's own requests are re-read under
                        // its lock: dedupe and the one-open-request rule
                        // are judged fresh, without touching another bundle.
                        let fresh: Vec<DecisionRequest> = self
                            .get_task_decision_requests(&task_id)?
                            .into_iter()
                            .filter(|request| {
                                request.gate == DecisionGate::AgentRequest
                                    && request.state == DecisionRequestState::Open
                            })
                            .collect();
                        if let Some(existing) = fresh.iter().find(|request| {
                            request.topic == topic
                                && request.summary.as_deref().map(question_key).as_deref()
                                    == Some(&key)
                        }) {
                            outcome = Some(AgentDecisionRaised {
                                request: existing.clone(),
                                deduplicated: true,
                            });
                            return Ok(());
                        }
                        if let Some(held) = fresh.first() {
                            return Err(OrbitError::InvalidInput(format!(
                                "task {task_id} already holds open decision request {}; wait for its answer before raising another",
                                held.id
                            )));
                        }
                        // Caps use the scanned snapshot with this task's
                        // slice replaced by the fresh read. Answers can
                        // only close requests in between, never open one,
                        // so the scanned count elsewhere never undercounts.
                        let elsewhere: Vec<&DecisionRequest> = open
                            .iter()
                            .filter(|request| request.task_id != task_id)
                            .collect();
                        let total = elsewhere.len() + fresh.len();
                        if total >= MAX_OPEN_AGENT_DECISIONS_PER_WORKSPACE {
                            return Err(OrbitError::PolicyDenied(format!(
                                "this workspace already holds {} open agent-raised decision requests (cap {MAX_OPEN_AGENT_DECISIONS_PER_WORKSPACE}); nothing was raised. Wait for the operator to answer some, or fold the question into an existing request",
                                total
                            )));
                        }
                        let by_filer = elsewhere
                            .iter()
                            .filter(|request| request.filer.as_deref() == Some(filer.as_str()))
                            .count()
                            + fresh
                                .iter()
                                .filter(|request| {
                                    request.filer.as_deref() == Some(filer.as_str())
                                })
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
                        .with_topic_and_filer(topic.clone(), filer.clone())
                        .with_category(&self.workspace_id()?);
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
                        // Only a request this call just wrote can auto-answer. The
                        // dedupe branch above returns the open request untouched.
                        let stored = self.auto_answer_from_precedent(task.status, &stored)?;
                        outcome = Some(AgentDecisionRaised {
                            request: stored,
                            deduplicated: false,
                        });
                        Ok(())
                    })?;
                Ok(())
            },
        )?;
        let raised = outcome.ok_or_else(|| {
            OrbitError::Execution(format!(
                "decision request on task {task_id} was not recorded"
            ))
        })?;
        Ok(raised)
    }
}

impl OrbitRuntime {
    pub(crate) fn raise_plugin_decision(
        &self,
        input: &Value,
        workspace_plugin_enabled: impl FnOnce(&str) -> Result<bool, OrbitError>,
    ) -> Result<Value, OrbitError> {
        let CallbackResolution::Identified(identity) =
            resolve_plugin_callback_session(self.env(), &self.global_root())?
        else {
            return Err(refuse(
                Refusal::CallerNotAllowed,
                "raising a plugin decision requires an authenticated plugin callback",
            ));
        };
        let plugin = identity.provenance.name.as_str();
        let input: RequestInput = serde_json::from_value(input.clone()).map_err(|error| {
            OrbitError::InvalidInput(format!("invalid plugin decision request: {error}"))
        })?;
        let raise = if input.withdraw {
            if input.apply.is_some()
                || input.summary.is_some()
                || input.answers.is_some()
                || input.recommended.is_some()
            {
                return Err(OrbitError::InvalidInput(
                    "withdraw takes only subject and an optional task_id".into(),
                ));
            }
            self.enabled_callback_plugin(&identity)?;
            None
        } else {
            Some(self.validate_plugin_raise(&input, &identity)?)
        };
        check_opaque("subject", &input.subject)?;

        self.ensure_coordination_task_write_permitted()
            .map_err(|error| refuse(Refusal::HostNotOwner, error.to_string()))?;
        let workspace_id = self.workspace_id()?;
        if identity.workspace_id.as_deref() != Some(workspace_id.as_str()) {
            return Err(refuse(
                Refusal::CallerNotAllowed,
                "the plugin callback belongs to another workspace",
            ));
        }
        if !self.plugin_load().is_active(plugin) || !workspace_plugin_enabled(plugin)? {
            return Err(refuse(
                Refusal::PluginUnavailable,
                "the plugin is not enabled in this workspace",
            ));
        }

        let lock = self.paths().orbit_dir.join("plugin-decision-raise");
        with_exclusive_file_lock(&lock, "plugin decision raise", || {
            let task = match input.task_id.as_deref() {
                Some(id) => Some(self.get_task(id)?),
                None => self.plugin_decision_holder(plugin)?,
            };
            let actor = format!("plugin:{plugin}");
            match raise {
                Some(raise) => self.record_plugin_raise(task, raise, &workspace_id, &actor),
                None => self.withdraw_plugin_decision(task, plugin, &input.subject, &actor),
            }
        })
    }

    pub(super) fn refresh_plugin_decision_states(
        &self,
        requests: &mut [DecisionRequest],
    ) -> Result<(), OrbitError> {
        let mut installed_digests = std::collections::BTreeMap::new();
        for request in requests.iter_mut().filter(|request| {
            matches!(
                request.state,
                DecisionRequestState::Open | DecisionRequestState::Expired
            )
        }) {
            let DecisionBinding::PluginObject {
                plugin,
                manifest_digest,
                ..
            } = &request.binding
            else {
                continue;
            };
            let digest = match installed_digests.entry(plugin.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                    self.installed_decision_plugin(plugin)?
                        .map(|row| row.manifest_digest),
                ),
            };
            if digest
                .as_ref()
                .is_some_and(|digest| digest != manifest_digest)
            {
                request.state = DecisionRequestState::Superseded;
            }
        }
        Ok(())
    }

    pub(crate) fn has_open_plugin_decision(&self, task: &Task) -> Result<bool, OrbitError> {
        let history = self.get_task_history(&task.id)?;
        Ok(!self.open_plugin_decisions_in(task, &history)?.is_empty())
    }

    fn record_plugin_raise(
        &self,
        task: Option<Task>,
        raise: Raise,
        workspace_id: &str,
        actor: &str,
    ) -> Result<Value, OrbitError> {
        if task.as_ref().is_some_and(|task| task.status.is_terminal()) {
            return Err(refuse(Refusal::RequestSuperseded, "the task is closed"));
        }
        // All raises on this workspace share the lock. Other task writes
        // can close existing requests while we scan; re-read this task
        // under its write lock before deciding deduplication or replacing.
        let open = self.list_decision_requests(Some(DecisionGate::PluginRequest), None, false)?;
        let task = match task {
            Some(task) => task,
            None => {
                check_caps(&open, &raise.apply.plugin, None)?;
                self.create_plugin_decision_holder(&raise.apply.plugin)?
            }
        };
        self.with_task_write_locked(&task.id, || {
            let current = self.current_plugin_decisions(&task.id)?;
            let existing = matching_request(&current, &raise.apply.plugin, &raise.apply.subject);
            if let Some(request) = existing
                && let DecisionBinding::PluginObject { revision, .. } = &request.binding
                && *revision == raise.apply.revision
            {
                return Ok(raised_json(request, true));
            }
            check_caps(
                &open,
                &raise.apply.plugin,
                existing.map(|request| request.id.as_str()),
            )?;

            let binding = DecisionBinding::PluginObject {
                plugin: raise.apply.plugin.clone(),
                subject: raise.apply.subject.clone(),
                revision: raise.apply.revision.clone(),
                manifest_digest: raise.apply.manifest_digest.clone(),
            };
            let request = DecisionRequest::new(
                task.id.clone(),
                DecisionGate::PluginRequest,
                raise.answers,
                binding,
                actor.to_owned(),
                chrono::Utc::now(),
            )
            .with_summary(raise.summary, raise.recommended)
            .with_category(workspace_id)
            .with_plugin_apply(raise.apply);
            let approvals = self.get_task_decision_requests(&task.id)?;
            let mut append_history = approvals
                .iter()
                .filter(|approval| {
                    approval.state == DecisionRequestState::Open
                        && matches!(
                            approval.gate,
                            DecisionGate::ProposedApproval | DecisionGate::ReviewApproval
                        )
                })
                .map(|approval| {
                    TaskHistoryEntry::new(request.raised_at, actor, DECISION_SUPERSEDED_EVENT)
                        .with_note(Some(approval.id.clone()))
                })
                .collect::<Vec<_>>();
            append_history.push(request.history_entry(actor));
            self.stores().task_records().update(
                &task.id,
                TaskRecordUpdateParams {
                    actor: actor.to_owned(),
                    append_history,
                    ..Default::default()
                },
            )?;
            Ok(raised_json(&request, false))
        })
    }

    fn withdraw_plugin_decision(
        &self,
        task: Option<Task>,
        plugin: &str,
        subject: &str,
        actor: &str,
    ) -> Result<Value, OrbitError> {
        let Some(task) = task else {
            return Ok(json!({"withdrawn": false}));
        };
        self.with_task_write_locked(&task.id, || {
            let current = self.current_plugin_decisions(&task.id)?;
            let Some(request) = matching_request(&current, plugin, subject) else {
                return Ok(json!({"withdrawn": false, "task_id": task.id}));
            };
            self.write_plugin_decision_event(
                &task.id,
                actor,
                TaskHistoryEntry::new(chrono::Utc::now(), actor, DECISION_WITHDRAWN_EVENT)
                    .with_note(Some(request.id.clone())),
            )?;
            Ok(json!({"withdrawn": true, "task_id": task.id, "id": request.id}))
        })
    }

    fn validate_plugin_raise(
        &self,
        input: &RequestInput,
        identity: &PluginCallbackIdentity,
    ) -> Result<Raise, OrbitError> {
        let apply = input.apply.as_ref().ok_or_else(|| {
            OrbitError::InvalidInput("apply is required when raising a plugin decision".into())
        })?;
        let manifest_digest = self.validate_plugin_apply_tool(&apply.tool, identity)?;
        let answers = validated_answers(
            input.answers.as_deref().unwrap_or_default(),
            input.recommended.as_deref(),
        )?;
        let summary = input
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty() && summary.chars().count() <= MAX_QUESTION_CHARS)
            .ok_or_else(|| OrbitError::InvalidInput("summary must be 1–600 characters".into()))?;
        if !apply.input.is_object()
            || apply.input.get("decision").is_some()
            || apply.input.to_string().len() > MAX_INPUT_BYTES
        {
            return Err(OrbitError::InvalidInput(
                concat!(
                    "apply.input must be a JSON object of at most 8 KiB ",
                    "without a top-level decision key"
                )
                .into(),
            ));
        }
        check_opaque("apply.revision", &apply.revision)?;
        Ok(Raise {
            summary: summary.to_owned(),
            recommended: input.recommended.clone(),
            answers,
            apply: PluginApply {
                plugin: identity.provenance.name.clone(),
                subject: input.subject.clone(),
                tool: apply.tool.clone(),
                manifest_digest,
                input: apply.input.clone(),
                revision: apply.revision.clone(),
                requested_by: identity.requested_by.clone(),
            },
        })
    }

    fn validate_plugin_apply_tool(
        &self,
        tool_name: &str,
        identity: &PluginCallbackIdentity,
    ) -> Result<String, OrbitError> {
        let plugin = &identity.provenance.name;
        if !namespace_collides_with_tool(plugin, false, tool_name)
            && !namespace_collides_with_tool(plugin, true, tool_name)
        {
            return Err(refuse(
                Refusal::CallerNotAllowed,
                "the apply tool belongs to another plugin",
            ));
        }
        let installed = self.enabled_callback_plugin(identity)?;
        if !namespace_collides_with_tool(plugin, installed.first_party, tool_name) {
            return Err(refuse(
                Refusal::CallerNotAllowed,
                "the apply tool belongs to another plugin",
            ));
        }
        crate::runtime::plugin::grants::verify_install_path(&self.global_root(), &installed)
            .map_err(|_| {
                refuse(
                    Refusal::PluginUnavailable,
                    "the recorded plugin install path is unavailable",
                )
            })?;
        let manifest = crate::runtime::plugin::cache::load_installed_plugin(&installed)
            .ok()
            .filter(|manifest| manifest.manifest_digest == installed.manifest_digest)
            .ok_or_else(|| {
                refuse(
                    Refusal::PluginUnavailable,
                    "the installed plugin manifest is unavailable or has changed",
                )
            })?;
        if !manifest.tools.iter().any(|tool| {
            manifest.tool_name(&tool.verb, installed.first_party) == tool_name
                && tool.execution_kind == PluginExecutionKind::Mutating
                && !tool.agent_callable
                && tool.mcp_scope == PluginMcpScope::None
        }) {
            return Err(refuse(
                Refusal::AnswerNotApplicable,
                "the apply tool must be declared mutating, agent_callable: false and mcp_scope: none",
            ));
        }
        Ok(installed.manifest_digest)
    }

    fn enabled_callback_plugin(
        &self,
        identity: &PluginCallbackIdentity,
    ) -> Result<InstalledPlugin, OrbitError> {
        let caller = &identity.provenance;
        self.installed_decision_plugin(&caller.name)?
            .filter(|plugin| plugin.enabled && plugin.manifest_digest == caller.manifest_digest)
            .ok_or_else(|| {
                refuse(
                    Refusal::PluginUnavailable,
                    "the callback's plugin is disabled or its installed manifest has changed",
                )
            })
    }

    fn installed_decision_plugin(
        &self,
        plugin: &str,
    ) -> Result<Option<InstalledPlugin>, OrbitError> {
        self.stores().host_store().read()?.plugin().get(plugin)
    }

    fn current_plugin_decisions(&self, id: &str) -> Result<Vec<DecisionRequest>, OrbitError> {
        let task = self.get_task(id)?;
        if task.status.is_terminal() {
            return Err(refuse(Refusal::RequestSuperseded, "the task is closed"));
        }
        self.open_plugin_decisions_in(&task, &self.get_task_history(id)?)
    }

    fn open_plugin_decisions_in(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
    ) -> Result<Vec<DecisionRequest>, OrbitError> {
        let mut requests = decision_requests_from_history(task, history, None, chrono::Utc::now());
        self.refresh_plugin_decision_states(&mut requests)?;
        requests.retain(|request| {
            request.gate == DecisionGate::PluginRequest
                && request.state == DecisionRequestState::Open
        });
        Ok(requests)
    }

    fn plugin_decision_holder(&self, plugin: &str) -> Result<Option<Task>, OrbitError> {
        Ok(self
            .list_task_metadata()?
            .into_iter()
            .find(|task| !task.status.is_terminal() && is_plugin_decision_holder(task, plugin)))
    }

    fn create_plugin_decision_holder(&self, plugin: &str) -> Result<Task, OrbitError> {
        self.add_task(TaskAddParams {
            title: holder_title(plugin),
            description: format!("Holds human decision requests raised by plugin {plugin}."),
            tags: vec![format!("plugin-request:{plugin}")],
            status: Some(TaskStatus::Someday),
            system_created: true,
            ..TaskAddParams::default()
        })
    }

    fn write_plugin_decision_event(
        &self,
        task_id: &str,
        actor: &str,
        entry: TaskHistoryEntry,
    ) -> Result<(), OrbitError> {
        self.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor: actor.to_owned(),
                append_history: vec![entry],
                ..Default::default()
            },
        )?;
        Ok(())
    }
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
