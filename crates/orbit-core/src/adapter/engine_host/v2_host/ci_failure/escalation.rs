//! Escalating a landing-branch failure that stays unfileable.
//!
//! Collection lists in `landing_evidence_incomplete` every landing-branch
//! failure whose evidence gaps outlasted the retry window. Deferring it again
//! would leave a red integration branch with no owner and a sweep that only
//! reports a note, so it is filed as a proposed "CI red, evidence incomplete"
//! task carrying the run URLs and the gaps, deduped per branch and workflow
//! against still-open tasks. It is not piloted: there is no diagnostic to
//! assess, and the task's first step is reading the run.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::DuplicateTaskLookup;
use crate::adapter::engine_host::v2_host::admission::sweep_filing::digest;
use crate::application::task::TaskAddParams;

use super::evidence::{bounded_error, retryable_pipeline_error};
use super::fields::{truncate_bytes, value_string};
use super::filing::{
    CI_FAILURE_KEY_TAG_PREFIX, CI_FAILURE_SWEEP_TITLE_PREFIX, CI_FAILURE_TAG,
    DESCRIPTION_LOG_BYTES, MAX_LISTED_RUNS,
};

/// Tag marking a task filed without a diagnosis.
const EVIDENCE_INCOMPLETE_TAG: &str = "ci-evidence-incomplete";

/// File one task per landing branch and workflow in the snapshot's
/// `landing_evidence_incomplete`, unless a still-open task already carries
/// that identity's key. Returns one entry per identity, `filed` or `existing`.
pub(super) fn file_landing_evidence_incomplete<L, F>(
    evidence: &Value,
    lookup: &L,
    audit: &Value,
    add_task: &mut F,
) -> Result<Vec<Value>, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    let mut groups: BTreeMap<(String, String), Vec<&Value>> = BTreeMap::new();
    for failure in evidence
        .get("landing_evidence_incomplete")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        groups
            .entry((
                value_string(failure, "head_branch"),
                value_string(failure, "workflow"),
            ))
            .or_default()
            .push(failure);
    }
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let lookup_error = |operation: &str, error: OrbitError| {
        retryable_pipeline_error(
            "task_creation",
            audit,
            vec![json!({
                "stage": "registration",
                "operation": operation,
                "retryable": true,
                "message": bounded_error(&error.to_string()),
            })],
        )
    };
    let tasks = lookup
        .list_tasks()
        .map_err(|error| lookup_error("find_evidence_incomplete_owner", error))?;
    let mut entries = Vec::new();
    for ((branch, workflow), failures) in groups {
        let failure_key = digest(&["ci-evidence-incomplete", &branch, &workflow]);
        let key_tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{failure_key}");
        let mut entry = json!({
            "failure_key": failure_key,
            "head_branch": branch,
            "workflow": workflow,
            "run_ids": failures.iter().filter_map(|failure| failure.get("run_id")).collect::<Vec<_>>(),
            "run_urls": failures.iter().filter_map(|failure| failure.get("url")).collect::<Vec<_>>(),
        });
        let existing = tasks.iter().find(|task| {
            matches!(
                task.status,
                TaskStatus::Proposed
                    | TaskStatus::Backlog
                    | TaskStatus::InProgress
                    | TaskStatus::Review
                    | TaskStatus::Blocked
            ) && task.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                && task.tags.iter().any(|tag| tag == &key_tag)
        });
        if let Some(task) = existing {
            entry["task_id"] = json!(task.id);
            entry["outcome"] = json!("existing");
            entries.push(entry);
            continue;
        }
        let task_id = add_task(TaskAddParams {
            title: format!(
                "{CI_FAILURE_SWEEP_TITLE_PREFIX}CI red on {branch}, evidence incomplete: {workflow}"
            ),
            description: description(&branch, &workflow, &failures),
            acceptance_criteria: vec![
                format!(
                    "The failing command in `{workflow}` on `{branch}` is identified from the \
                     linked run log and repaired, or a diagnosed CI-failure task owns it"
                ),
                format!("A later `{workflow}` run on `{branch}` completes without that failure"),
            ],
            tags: vec![
                CI_FAILURE_TAG.to_string(),
                key_tag,
                EVIDENCE_INCOMPLETE_TAG.to_string(),
                "github-actions".to_string(),
            ],
            required_tools: Vec::new(),
            crew: None,
            priority: TaskPriority::High,
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Bug),
            // Quarantined like every sweep filing; it carries no diagnosis a
            // pilot could assess, so an operator triages it from the run.
            status: Some(TaskStatus::Proposed),
            system_created: true,
            ..TaskAddParams::default()
        })
        .map_err(|error| lookup_error("orbit.task.add", error))?;
        entry["task_id"] = json!(task_id);
        entry["outcome"] = json!("filed");
        entries.push(entry);
    }
    Ok(entries)
}

fn description(branch: &str, workflow: &str, failures: &[&Value]) -> String {
    let sweeps = failures
        .iter()
        .filter_map(|failure| failure["consecutive_sweeps"].as_u64())
        .max()
        .unwrap_or_default();
    let mut text = format!(
        "CI on landing branch `{branch}` is red in workflow `{workflow}`, and the CI-failure \
         sweep could not collect evidence complete enough to file a diagnosed repair for \
         {sweeps} consecutive sweeps. Retrying was not closing the gap, so the sweep escalates \
         it here instead of deferring it again.\n\n## Failing runs\n\n"
    );
    for failure in failures.iter().take(MAX_LISTED_RUNS) {
        let job = failure["failed_jobs"][0]["name"].as_str().unwrap_or("-");
        text.push_str(&format!(
            "- Run `{}`: {} (job `{job}`, event `{}`, reported head `{}`)\n",
            value_string(failure, "run_id"),
            value_string(failure, "url"),
            value_string(failure, "event"),
            value_string(failure, "reported_head_sha"),
        ));
    }
    if failures.len() > MAX_LISTED_RUNS {
        text.push_str(&format!(
            "- … and {} more run(s)\n",
            failures.len() - MAX_LISTED_RUNS
        ));
    }
    text.push_str("\n## Evidence gaps\n\n");
    for failure in failures.iter().take(MAX_LISTED_RUNS) {
        for gap in failure["evidence_gaps"].as_array().into_iter().flatten() {
            text.push_str(&format!(
                "- `{}` (run `{}`, job `{}`, {} consecutive sweeps): {}\n",
                value_string(gap, "operation"),
                value_string(gap, "run_id"),
                value_string(gap, "job_id"),
                value_string(gap, "consecutive_sweeps"),
                value_string(gap, "message"),
            ));
        }
    }
    if let Some(excerpt) = failures
        .iter()
        .map(|failure| value_string(failure, "log_excerpt"))
        .find(|excerpt| !excerpt.is_empty())
    {
        text.push_str(&format!(
            "\n## Partial log excerpt\n\n```\n{}\n```\n",
            truncate_bytes(&excerpt, DESCRIPTION_LOG_BYTES)
        ));
    }
    text.push_str(
        "\n## Next step\n\nOpen the run URL and read the failed job's log to find the failing \
         command, then repair it. If a later sweep files that failure as its own diagnosed task, \
         close this one as covered by it.\n",
    );
    text
}
