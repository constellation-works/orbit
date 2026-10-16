//! Authenticate a stopped repair's branch adoption without reusing authority.
use super::*;

impl OrbitRuntime {
    pub(super) fn source_repair_adopted_binding(
        &self,
        source: &JobRun,
        task: &Task,
        worktree: &Value,
        adopted: &Value,
    ) -> Result<SourceRepairBinding, OrbitError> {
        let input = source
            .input
            .as_ref()
            .ok_or_else(|| refuse("repair source input missing"))?;
        let admission: SourceRepairAdmission =
            serde_json::from_value(input[SOURCE_REPAIR_KEY].clone())
                .map_err(|error| refuse(format!("unreadable prior repair admission: {error}")))?;
        let binding = &admission.binding;
        let entries = self.get_task_history(&task.id)?;
        let reservations = orbit_types::workflow::source_repair_reservations(&entries)
            .map_err(|error| refuse(format!("unreadable repair reservation: {error}")))?;
        let matching = reservations
            .iter()
            .filter(|record| record.request_key == admission.request_key)
            .collect::<Vec<_>>();
        let recorded =
            serde_json::to_value(&admission).map_err(|error| refuse(error.to_string()))?;
        // Run input alone is not operator authority. Require the immutable,
        // system-authored reservation and its unique link to this exact run.
        let reserved = entries.iter().any(|entry| {
            entry.by == "system"
                && entry.event == SOURCE_REPAIR_RESERVED
                && entry.note.as_deref().is_some_and(|note| {
                    serde_json::from_str::<Value>(note).is_ok_and(|value| value == recorded)
                })
        });
        let links = entries
            .iter()
            .filter(|entry| entry.event == SOURCE_REPAIR_LINKED)
            .map(|entry| {
                serde_json::from_str::<RepairLink>(entry.note.as_deref().unwrap_or_default())
                    .map(|link| (entry.by.as_str(), link))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| refuse(format!("unreadable prior repair link: {error}")))?;
        let linked = links
            .iter()
            .filter(|(_, link)| link.request_key == admission.request_key)
            .collect::<Vec<_>>();
        if matching.len() != 1
            || digest(matching[0])? != digest(&admission)?
            || !reserved
            || linked.len() != 1
            || linked[0].0 != "system"
            || linked[0].1.run_id != source.run_id
            || binding.schema_version != 1
            || binding.workspace_id != self.workspace_id()?
            || binding.task_id != task.id
            || self.automation_machine_identity() != Some(binding.machine_id.as_str())
            || source.retry_source_run_id.as_deref() != Some(binding.source_run_id.as_str())
            || binding.source_run_id == source.run_id
            || input["completion"] != "review"
            || input["auto_fix"] != false
            || input[orbit_types::workflow::REVIEW_ADMISSION_KEY]
                != serde_json::to_value(&admission.review)
                    .map_err(|error| refuse(error.to_string()))?
        {
            return Err(refuse("prior repair admission or run link changed"));
        }
        if admitted_task_digest(task, &entries, &source.run_id, admission.authorized_at)?
            != binding.task_digest
        {
            return Err(refuse(
                "task meaning or scope changed since original repair admission",
            ));
        }
        let predecessor = self
            .get_job_run_backend(&binding.source_run_id)?
            .ok_or_else(|| refuse("prior repair source missing"))?;
        let prior_state = self
            .read_run_state(&binding.source_run_id)?
            .ok_or_else(|| refuse("prior repair source checkpoints missing"))?;
        let definition = self.source_repair_definition(&predecessor)?;
        if digest(
            &json!({"run":predecessor,"steps":predecessor.steps,"checkpoints":prior_state,"definition":definition}),
        )? != binding.source_digest
        {
            return Err(refuse("immutable prior repair source changed"));
        }
        if field(adopted, "phase")? != "candidate_resume"
            || field(adopted, "outcome")? != "resumed_repaired"
            || adopted.get("implement").and_then(Value::as_bool) != Some(true)
            || field(adopted, "source_run_id")? != binding.source_run_id
            || field(adopted, "source_branch")? != binding.branch
            || field(adopted, "source_sha")? != binding.head_sha
            || adopted
                .get("source_machine_id")
                .is_none_or(|value| !value.is_null())
            || field(adopted, "base_sha")? != field(worktree, "base_sha")?
            || field(adopted, "reused_branch")? != binding.branch
            || field(adopted, "reused_head_sha")? != binding.head_sha
            || field(adopted, "prior_pull_request")? != binding.pull_request
            || field(adopted, "prior_pull_request_head")? != binding.branch
            || adopted["repair"]["failed_step_id"] != "operator_source_repair"
        {
            return Err(refuse(
                "successful branch adoption disagrees with prior repair admission",
            ));
        }
        Ok(admission.binding)
    }
}

/// Compare the admitted task while accounting only for this run's durable host
/// additions. The task and the original reservation remain immutable here.
pub(super) fn admitted_task_digest(
    task: &Task,
    entries: &[TaskHistoryEntry],
    run_id: &str,
    authorized_at: chrono::DateTime<Utc>,
) -> Result<String, OrbitError> {
    // Delivery legitimately appends selectors for changed files. Account
    // only for this attempt's system-recorded additions in memory; arbitrary
    // scope edits still change the original digest and no history is edited.
    let mut original_task = task.clone();
    for entry in entries.iter().rev().filter(|entry| {
        entry.by == "system"
            && entry.at >= authorized_at
            && entry.event == orbit_types::task::CONTEXT_FILES_WIDENED_EVENT
    }) {
        let widening = entry
            .note
            .as_deref()
            .and_then(orbit_types::task::ContextFilesWidening::from_note)
            .ok_or_else(|| refuse("unreadable host selector widening"))?;
        if widening.run_id == run_id {
            for selector in widening.selectors {
                let index = original_task
                    .context_files
                    .iter()
                    .position(|s| s == &selector)
                    .ok_or_else(|| refuse("host-widened selector was removed"))?;
                original_task.context_files.remove(index);
            }
        }
    }
    task_digest(&original_task)
}
