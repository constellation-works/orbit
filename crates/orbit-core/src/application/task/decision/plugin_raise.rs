//! Plugin decision request shapes, bounds and the pure helpers their raises
//! and withdrawals use. The operations themselves are in `raise.rs`.

use std::collections::BTreeSet;

use orbit_common::{DecisionRefusalCode as Refusal, OrbitError};
use orbit_types::task::{
    AGENT_DECISION_MAX_OPTIONS, AGENT_DECISION_MIN_OPTIONS, DecisionAnswer, DecisionBinding,
    DecisionRequest, PluginApply, Task, decision_answer_line,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::listing::decision_request_json;

pub(super) const MAX_INPUT_BYTES: usize = 8 * 1024;

const MAX_OPEN_PER_WORKSPACE: usize = 50;

const MAX_OPEN_PER_PLUGIN: usize = 20;

#[derive(Deserialize)]
pub(super) struct RequestInput {
    pub(super) task_id: Option<String>,
    pub(super) subject: String,
    #[serde(default)]
    pub(super) withdraw: bool,
    pub(super) summary: Option<String>,
    pub(super) recommended: Option<String>,
    pub(super) answers: Option<Vec<AnswerInput>>,
    pub(super) apply: Option<ApplyInput>,
}

#[derive(Deserialize)]
pub(super) struct AnswerInput {
    id: String,
    label: String,
    consequence: String,
}

#[derive(Deserialize)]
pub(super) struct ApplyInput {
    pub(super) tool: String,
    pub(super) revision: String,
    pub(super) input: Value,
}

pub(super) struct Raise {
    pub(super) summary: String,
    pub(super) recommended: Option<String>,
    pub(super) answers: Vec<DecisionAnswer>,
    pub(super) apply: PluginApply,
}

pub(super) fn validated_answers(
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

pub(super) fn check_opaque(field: &str, value: &str) -> Result<(), OrbitError> {
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
    task.created_by.as_deref() == Some(crate::application::task::helpers::SYSTEM_ACTOR_LABEL)
        && task.title == holder_title(plugin)
        && task.tags.contains(&format!("plugin-request:{plugin}"))
}

pub(super) fn holder_title(plugin: &str) -> String {
    format!("Decisions raised by plugin {plugin}")
}

pub(super) fn matching_request<'a>(
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

pub(super) fn check_caps(
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
        plugin_count += usize::from(matches!(
            &request.binding,
            DecisionBinding::PluginObject { plugin: owner, .. } if owner == plugin
        ));
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

pub(super) fn raised_json(request: &DecisionRequest, deduplicated: bool) -> Value {
    let mut value = decision_request_json(request);
    value["deduplicated"] = json!(deduplicated);
    value
}

pub(super) fn refuse(code: Refusal, message: impl Into<String>) -> OrbitError {
    OrbitError::decision_refused(code, message)
}
