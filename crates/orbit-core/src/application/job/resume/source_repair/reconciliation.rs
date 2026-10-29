//! Reconcile immutable reservation evidence without spending a second attempt.
use super::super::plan::RESUME_LINEAGE_SCAN_LIMIT;
use super::*;

/// Permit only filling the missing lifecycle prerequisite. Reconstruct the
/// two unauthored plan forms in memory and compare the entire original task
/// digest; never write the old plan back or ignore any other meaning field.
pub(super) fn validate_source_repair_plan_correction(
    task: &Task,
    original: &SourceRepairAdmission,
    binding: &SourceRepairBinding,
) -> Result<(), OrbitError> {
    crate::application::task::ensure_task_has_execution_plan(&task.id, &task.plan)?;
    let mut unchanged = binding.clone();
    unchanged.task_digest = original.binding.task_digest.clone();
    if unchanged != original.binding {
        return Err(refuse(
            "reconciliation cannot change source, PR, head, lease or stopped lineage",
        ));
    }
    let mut before = task.clone();
    for plan in ["", "To be authored by executing agent at start time."] {
        before.plan = plan.into();
        if task_digest(&before)? == original.binding.task_digest {
            return Ok(());
        }
    }
    Err(refuse(
        "reconciliation permits only correction of an empty or placeholder plan; unrelated task meaning changed",
    ))
}

impl OrbitRuntime {
    pub(super) fn source_repair_reservation_digests(
        &self,
        task: &str,
    ) -> Result<Value, OrbitError> {
        let records =
            orbit_types::workflow::source_repair_reservations(&self.get_task_history(task)?)
                .map_err(|e| refuse(e.to_string()))?;
        let mut digests = serde_json::Map::new();
        for record in records {
            if digests
                .insert(record.request_key.clone(), json!(digest(&record)?))
                .is_some()
            {
                return Err(refuse("duplicate reservation keys"));
            }
        }
        Ok(Value::Object(digests))
    }

    /// A missing post-insert link is not evidence that no run was inserted.
    /// Cross-check every bounded child, refusing ambiguity and truncated scans.
    pub(super) fn source_repair_linked_run(
        &self,
        task: &str,
        source: &str,
        key: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        let entries = self.get_task_history(task)?;
        let links = entries
            .iter()
            .filter(|e| e.event == SOURCE_REPAIR_LINKED)
            .map(|e| {
                if e.by != "system" {
                    return Err(refuse("unauthenticated repair link"));
                }
                serde_json::from_str::<RepairLink>(e.note.as_deref().unwrap_or_default())
                    .map_err(|e| refuse(format!("unreadable repair link: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ids = links
            .iter()
            .filter(|l| l.request_key == key)
            .map(|l| l.run_id.clone())
            .collect::<BTreeSet<_>>();
        if ids.len() > 1 || links.iter().filter(|link| link.request_key == key).count() > 1 {
            return Err(refuse("repair key has inconsistent run links"));
        }
        let children = self
            .stores()
            .jobs()
            .job_run_retries(source, RESUME_LINEAGE_SCAN_LIMIT)?;
        if children.len() == RESUME_LINEAGE_SCAN_LIMIT {
            return Err(refuse(
                "repair linkage exceeds the bounded reconciliation window",
            ));
        }
        if children.iter().any(|run| {
            run.input
                .as_ref()
                .and_then(|input| input.get(SOURCE_REPAIR_KEY))
                .is_some_and(|record| record.get("request_key").and_then(Value::as_str).is_none())
        }) {
            return Err(refuse("repair child has unreadable request identity"));
        }
        let mut found = children.into_iter().filter(|run| {
            run.input
                .as_ref()
                .is_some_and(|i| i[SOURCE_REPAIR_KEY]["request_key"] == key)
        });
        let child = found.next();
        if found.next().is_some() {
            return Err(refuse("repair key has ambiguous run evidence"));
        }
        if let Some(id) = ids.into_iter().next() {
            let run = self
                .get_job_run_backend(&id)?
                .ok_or_else(|| refuse("linked repair run is missing"))?;
            if child.as_ref().is_none_or(|c| c.run_id != id) {
                return Err(refuse("linked repair and child evidence disagree"));
            }
            return Ok(Some(run));
        }
        Ok(child)
    }
}
