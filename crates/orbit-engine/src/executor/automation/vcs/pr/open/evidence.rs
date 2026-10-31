use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::TaskHistoryEntry;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::super::super::input::input_string_field;
use super::super::super::git::git_output;
use super::super::super::handoff::HandoffContext;

/// Borrowed fields used to admit and deduplicate either kind of evidence.
pub(super) struct ValidationRecord<'a> {
    pub(super) command: &'a str,
    pub(super) run_id: &'a str,
    pub(super) base_sha: &'a str,
    pub(super) tested_head: &'a str,
    pub(super) valid: bool,
}

/// Keep the latest matching record per command, from delivered tasks' history
/// followed by a claimed leaf's handed validation output. Claimed leaves cannot
/// write the owner's task history.
pub(super) fn collect_validation_records<H: RuntimeHost + ?Sized, T: DeserializeOwned>(
    host: &H,
    context: &HandoffContext,
    base_sha: &str,
    input: &Value,
    (event, key): (&str, &str),
    parse: fn(&TaskHistoryEntry) -> Option<T>,
    fields: impl Fn(&T) -> ValidationRecord<'_>,
) -> Vec<T> {
    let kind = key.replace('_', "-");
    let head = match git_output(&context.workspace_path, &["rev-parse", "HEAD"]) {
        Ok(head) => head,
        Err(error) => {
            tracing::warn!("pr_open could not read HEAD for {kind} validation: {error}");
            return Vec::new();
        }
    };
    let mut records = BTreeMap::new();
    let mut insert_matching = |record| {
        let evidence = fields(&record);
        if evidence.valid
            && evidence.run_id == context.batch_id
            && evidence.base_sha == base_sha
            && evidence.tested_head == head
        {
            records.insert(evidence.command.to_string(), record);
        }
    };
    for task in &context.tasks {
        let history = match host.get_task_history(&task.id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    task_id = task.id,
                    "pr_open could not read task history for {kind} validation: {error}"
                );
                continue;
            }
        };
        for record in history
            .iter()
            .filter(|entry| entry.event == event)
            .filter_map(parse)
        {
            insert_matching(record);
        }
    }
    if let Some(handed) = input
        .get("claim_validation")
        .and_then(|validation| validation.get(key))
        .filter(|value| !value.is_null())
    {
        match serde_json::from_value::<Vec<T>>(handed.clone()) {
            Ok(handed) => {
                for record in handed {
                    insert_matching(record);
                }
            }
            Err(error) => tracing::warn!("pr_open ignored a malformed {key} input: {error}"),
        }
    }
    records.into_values().collect()
}

pub(super) fn append_section(body: String, section: Option<String>) -> String {
    match section {
        Some(section) => format!("{}\n\n{section}\n", body.trim_end()),
        None => body,
    }
}

/// Append the settled review's sections — fixes [ORB-13989], raw validation
/// evidence, and validation limits [ORB-14192] — to the PR body, generated or
/// supplied, so the reviewer commit and what the review established are
/// visible where it is published. Without any applicable sections the body
/// is unchanged.
pub(super) fn with_review_fixes(body: String, input: &Value) -> String {
    append_section(
        body,
        input_string_field(input, "review_fixes")
            .map(|section| section.trim().to_string())
            .filter(|section| !section.is_empty()),
    )
}

/// Refuse to publish when the checked-out head or the pinned base differ from
/// the candidate the review gate settled. An empty `reviewed_head_sha` means
/// no gate applied to this run.
pub(in crate::executor::automation::vcs) fn ensure_reviewed_candidate(
    workspace_path: &std::path::Path,
    input: &Value,
    base_sha: &str,
) -> Result<(), OrbitError> {
    let Some(reviewed_head) = input_string_field(input, "reviewed_head_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head_sha != reviewed_head {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: checked-out head {head_sha} is not the reviewed candidate \
             {reviewed_head}; the gate must settle the current candidate before a PR is opened"
        )));
    }
    if let Some(reviewed_base) = input_string_field(input, "reviewed_base_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        && reviewed_base != base_sha
    {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: base {base_sha} is not the reviewed base {reviewed_base}; a \
             candidate rebased onto a different base needs a fresh review"
        )));
    }
    Ok(())
}
