//! Authenticated plugin decision raises and withdrawals.

use std::collections::BTreeSet;

use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::{DecisionRefusalCode as Refusal, OrbitError};
use orbit_tools::plugin::{
    CallbackResolution, PluginCallbackIdentity, resolve_plugin_callback_session,
};
use orbit_types::plugin::{
    InstalledPlugin, PluginExecutionKind, PluginMcpScope, namespace_collides_with_tool,
};
use orbit_types::task::{
    AGENT_DECISION_MAX_OPTIONS, AGENT_DECISION_MIN_OPTIONS, DECISION_WITHDRAWN_EVENT,
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState,
    PluginApply, Task, TaskHistoryEntry, TaskStatus, decision_answer_line,
    decision_requests_from_history,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{TaskAddParams, TaskRecordUpdateParams, TaskRecords};
use super::listing::decision_request_json;
use crate::OrbitRuntime;

const MAX_INPUT_BYTES: usize = 8 * 1024;
const MAX_OPEN_PER_WORKSPACE: usize = 50;
const MAX_OPEN_PER_PLUGIN: usize = 20;

#[derive(Deserialize)]
struct RequestInput {
    task_id: Option<String>,
    subject: String,
    #[serde(default)]
    withdraw: bool,
    summary: Option<String>,
    recommended: Option<String>,
    answers: Option<Vec<AnswerInput>>,
    apply: Option<ApplyInput>,
}

#[derive(Deserialize)]
struct AnswerInput {
    id: String,
    label: String,
    consequence: String,
}

#[derive(Deserialize)]
struct ApplyInput {
    tool: String,
    revision: String,
    input: Value,
}

struct Raise {
    summary: String,
    recommended: Option<String>,
    answers: Vec<DecisionAnswer>,
    apply: PluginApply,
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
                && matches!(&request.binding, DecisionBinding::PluginObject { revision, .. } if revision == &raise.apply.revision)
            {
                return Ok(raised_json(request, true));
            }
            check_caps(&open, &raise.apply.plugin, existing.map(|request| request.id.as_str()))?;

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
            self.write_plugin_decision_event(&task.id, actor, request.history_entry(actor))?;
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
            .filter(|summary| {
                !summary.is_empty() && summary.chars().count() <= super::raise::MAX_QUESTION_CHARS
            })
            .ok_or_else(|| OrbitError::InvalidInput("summary must be 1–600 characters".into()))?;
        if !apply.input.is_object()
            || apply.input.get("decision").is_some()
            || apply.input.to_string().len() > MAX_INPUT_BYTES
        {
            return Err(OrbitError::InvalidInput(
                "apply.input must be a JSON object of at most 8 KiB without a top-level decision key".into(),
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

fn validated_answers(
    answers: &[AnswerInput],
    recommended: Option<&str>,
) -> Result<Vec<DecisionAnswer>, OrbitError> {
    if !(AGENT_DECISION_MIN_OPTIONS..=AGENT_DECISION_MAX_OPTIONS).contains(&answers.len()) {
        return Err(OrbitError::InvalidInput(
            "a plugin decision requires two to four answers".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    let answers = answers
        .iter()
        .enumerate()
        .map(|(index, answer)| {
            let mut bytes = answer.id.bytes();
            if answer.id.len() > 32
                || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                || !bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || !ids.insert(answer.id.as_str())
            {
                return Err(OrbitError::InvalidInput(
                    "answer ids must be unique and match [a-z][a-z0-9_]{0,31}".into(),
                ));
            }
            let label = decision_answer_line(&answer.label, &format!("answer {} label", index + 1))
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            let consequence = decision_answer_line(
                &answer.consequence,
                &format!("answer {} consequence", index + 1),
            )
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            Ok(DecisionAnswer::plain(&answer.id, label, consequence))
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;
    if recommended.is_some_and(|id| !ids.contains(id)) {
        return Err(OrbitError::InvalidInput(
            "recommended must name an answer id".into(),
        ));
    }
    Ok(answers)
}

fn check_opaque(field: &str, value: &str) -> Result<(), OrbitError> {
    if value.is_empty()
        || value.chars().count() > 128
        || value
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    {
        return Err(OrbitError::InvalidInput(format!(
            "{field} must be 1–128 printable characters"
        )));
    }
    Ok(())
}

pub(crate) fn is_plugin_decision_holder(task: &Task, plugin: &str) -> bool {
    task.created_by.as_deref() == Some(super::super::helpers::SYSTEM_ACTOR_LABEL)
        && task.title == holder_title(plugin)
        && task.tags.contains(&format!("plugin-request:{plugin}"))
}

fn holder_title(plugin: &str) -> String {
    format!("Decisions raised by plugin {plugin}")
}

fn matching_request<'a>(
    requests: &'a [DecisionRequest],
    plugin: &str,
    subject: &str,
) -> Option<&'a DecisionRequest> {
    requests.iter().find(|request| {
        matches!(
            &request.binding,
            DecisionBinding::PluginObject { plugin: owner, subject: object, .. }
                if owner == plugin && object == subject
        )
    })
}

fn check_caps(
    open: &[DecisionRequest],
    plugin: &str,
    replacing: Option<&str>,
) -> Result<(), OrbitError> {
    let mut workspace_count = 0;
    let mut plugin_count = 0;
    for request in open
        .iter()
        .filter(|request| replacing != Some(request.id.as_str()))
    {
        workspace_count += 1;
        plugin_count += usize::from(
            matches!(&request.binding, DecisionBinding::PluginObject { plugin: owner, .. } if owner == plugin),
        );
    }
    if plugin_count >= MAX_OPEN_PER_PLUGIN {
        return Err(OrbitError::PolicyDenied(format!(
            "plugin {plugin} has reached the cap of {MAX_OPEN_PER_PLUGIN} open decision requests"
        )));
    }
    if workspace_count >= MAX_OPEN_PER_WORKSPACE {
        return Err(OrbitError::PolicyDenied(format!(
            "the workspace has reached the cap of {MAX_OPEN_PER_WORKSPACE} open plugin decision requests"
        )));
    }
    Ok(())
}

fn raised_json(request: &DecisionRequest, deduplicated: bool) -> Value {
    let mut value = decision_request_json(request);
    value["deduplicated"] = json!(deduplicated);
    value
}

fn refuse(code: Refusal, message: impl Into<String>) -> OrbitError {
    OrbitError::decision_refused(code, message)
}
