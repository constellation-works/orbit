//! Shadow task-quality consumer. The committed task is the reconciliation
//! source; a failed local scheduling write cannot lose work permanently.
mod contract;
mod ledger;

use chrono::Utc;
use orbit_common::{OrbitError, fs::io};
use orbit_store::contracts::assistance::AssistanceAdmission;
use orbit_types::assistance::{self, AssistanceDisposition, AssistanceReason};
use orbit_types::task::Task;

pub use contract::{TASK_QUALITY_USE_CASE, task_quality_question_template};
pub use ledger::{
    TaskQualityAnnotation, TaskQualityDismissal, TaskQualityDismissalReason, TaskQualityHistory,
    TaskQualitySnapshot,
};

use super::assistance::AssistanceService;
use crate::OrbitRuntime;

/// Drop a restored snapshot's stale admission receipt so the owner tick can
/// mint a fresh attempt. The snapshot keeps its content-derived identity — and
/// with it its completed results, annotations and dismissal identities — while
/// the old work ID (whose assistance slot moved on to superseding content)
/// must never be mistaken for current coverage. Expiry or retention alone is
/// a bounded coverage gap, not evidence that semantic content was restored.
fn refresh_restored_snapshot(
    service: &AssistanceService,
    snapshot: &mut ledger::TaskQualitySnapshot,
    semantic_restoration: bool,
) -> Result<(), OrbitError> {
    let Some(work) = snapshot.work_id.as_deref() else {
        return Ok(());
    };
    if !semantic_restoration && !service.assistance_work_was_superseded(work)? {
        return Ok(());
    }
    let stale = match service.assistance_assessment(work, &snapshot.source_digest)? {
        None => true,
        Some(record) => {
            !record.current
                || record
                    .result
                    .as_ref()
                    .is_some_and(|r| r.disposition == AssistanceDisposition::Stale)
        }
    };
    if stale {
        snapshot.work_id = None;
        snapshot.admission_started = false;
        snapshot.coverage_gap = None;
        snapshot.collected_at = Utc::now();
    }
    Ok(())
}

// Always acquire the task's shared lock before its private consumer lock.
// This fences terminal transitions/semantic edits during local admission or
// feedback, while permitting claimed tasks to be assessed without task writes.
fn with_quality_subject<T>(
    runtime: &OrbitRuntime,
    id: &str,
    body: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let mut body = Some(body);
    let mut result = None;
    runtime.stores().tasks().with_task_read_lock(id, &mut || {
        let body = body
            .take()
            .ok_or_else(|| OrbitError::Store("task quality lock entered twice".into()))?;
        result = Some(body()?);
        Ok(())
    })?;
    result.ok_or_else(|| OrbitError::Store("task quality lock body did not run".into()))
}

impl OrbitRuntime {
    /// Local, best-effort post-commit marker. The owner clock repairs missing
    /// markers by reading committed tasks. No adapter/credential/network call.
    pub(crate) fn schedule_task_quality_after_commit(&self, task: &Task) {
        let result = self
            .task_quality_service()
            .and_then(|service| self.schedule_task_quality_with_service(&task.id, &service));
        if result.is_err() {
            // Do not log task prose, settings, credentials or provider errors.
            tracing::debug!("task quality scheduling deferred to owner reconciliation");
        }
    }

    fn schedule_task_quality_with_service(
        &self,
        id: &str,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace_id()?;
        let Some(epoch) = service.assistance_policy_epoch(&workspace, TASK_QUALITY_USE_CASE)?
        else {
            return Ok(());
        };
        self.ensure_coordination_task_write_permitted()?;
        let path = ledger::quality_ledger_path(self, id)?;
        with_quality_subject(self, id, || {
            ledger::with_quality_history(&path, || {
                // Read latest committed state *inside* the consumer lock: an old
                // create reply or delayed edit hook cannot supersede a newer edit.
                let task = self.get_task(id)?;
                if task.status.is_terminal() {
                    return Ok(());
                }
                let digest = contract::task_quality_digest(&task)?;
                let snapshot_id = assistance::assistance_digest(
                    &serde_json::to_vec(&(
                        "task-quality-snapshot/1",
                        &workspace,
                        id,
                        &digest,
                        &epoch,
                    ))
                    .map_err(ledger::quality_json_error)?,
                );
                let mut history = ledger::read_quality_history(&path)?;
                // A missing current projection can mean expiry, retention or
                // an off switch. Only a different selected snapshot proves a
                // semantic restoration; otherwise require a displaced slot.
                let semantic_restoration = history
                    .current_id
                    .as_ref()
                    .is_some_and(|current| current != &snapshot_id);
                if !history.snapshots.iter().any(|s| s.id == snapshot_id) {
                    history.snapshots.push(TaskQualitySnapshot {
                        id: snapshot_id.clone(),
                        source_digest: digest,
                        policy_epoch: epoch,
                        collected_at: Utc::now(),
                        admission_started: false,
                        work_id: None,
                        coverage_gap: None,
                        result: None,
                        annotations: Vec::new(),
                    });
                    if history.snapshots.len() > 32 {
                        history.snapshots.remove(0);
                    }
                } else if let Some(snapshot) =
                    history.snapshots.iter_mut().find(|s| s.id == snapshot_id)
                {
                    // Restoring prior semantic content reuses its snapshot (and
                    // its annotation/dismissal identities), but the assistance
                    // slot moved on when the superseding content was admitted.
                    // Refresh the stale receipt so the owner tick mints a
                    // fresh attempt instead of waiting on superseded work.
                    refresh_restored_snapshot(service, snapshot, semantic_restoration)?;
                }
                history.current_id = Some(snapshot_id);
                ledger::write_quality_history(&path, &history)
            })
        })
    }

    /// Private history, including shadow results and reasoned feedback. Every
    /// read revalidates semantic content and policy; no normal task projection.
    pub fn task_quality_history(
        &self,
        id: &str,
        service: &AssistanceService,
    ) -> Result<TaskQualityHistory, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let path = ledger::quality_ledger_path(self, id)?;
        with_quality_subject(self, id, || {
            ledger::with_quality_history(&path, || {
                let task = self.get_task(id)?;
                let digest = contract::task_quality_digest(&task)?;
                let epoch = service
                    .assistance_policy_epoch(&self.workspace_id()?, TASK_QUALITY_USE_CASE)?;
                let mut history = ledger::read_quality_history(&path)?;
                let mut stale_ids = Vec::new();
                for snapshot in &mut history.snapshots {
                    if let Some(work) = &snapshot.work_id {
                        let Some(record) =
                            service.assistance_assessment(work, &snapshot.source_digest)?
                        else {
                            stale_ids.push(snapshot.id.clone());
                            continue;
                        };
                        // Queue age settles still-queued work as an explicit gap.
                        // That row stays readable, but it is not current coverage.
                        let queue_expired = record
                            .result
                            .as_ref()
                            .is_some_and(|result| result.reason == AssistanceReason::QueueExpired);
                        if !record.current
                            || queue_expired
                            || record.result.as_ref().is_some_and(|result| {
                                result.disposition == AssistanceDisposition::Stale
                            })
                        {
                            stale_ids.push(snapshot.id.clone());
                        }
                        // Retention/off switches may make the service view stale;
                        // preserve completed history and its feedback verbatim.
                        if let Some(result) = record.result
                            && (snapshot.result.is_none()
                                || result.disposition != AssistanceDisposition::Stale)
                        {
                            snapshot.annotations = ledger::quality_annotations(snapshot, &result)?;
                            snapshot.result = Some(result);
                        }
                    }
                }
                // Retain terminal-task history, but don't present replies that
                // arrived after a terminal transition as a fresh clean assessment.
                history.current_id = history.current_id.filter(|id| {
                    history.snapshots.iter().any(|s| {
                        &s.id == id
                            && s.source_digest == digest
                            && epoch.as_ref() == Some(&s.policy_epoch)
                            && !stale_ids.contains(id)
                    })
                });
                if path.exists() {
                    ledger::write_quality_history(&path, &history)?;
                }
                Ok(history)
            })
        })
    }

    /// Feedback is private data, not a task edit or adjudicated human label.
    /// Both snapshot and annotation identity are required to prevent dismissing
    /// a different criterion after a concurrent edit or reorder.
    pub fn dismiss_task_quality_annotation(
        &self,
        id: &str,
        snapshot_id: &str,
        annotation_id: &str,
        reason: TaskQualityDismissalReason,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let path = ledger::quality_ledger_path(self, id)?;
        with_quality_subject(self, id, || {
            ledger::with_quality_history(&path, || {
                let mut history = self.task_quality_history(id, service)?;
                if history.current_id.as_deref() != Some(snapshot_id) {
                    return Err(OrbitError::InvalidInput(
                        "task quality snapshot is stale".into(),
                    ));
                }
                let annotation = history
                    .snapshots
                    .iter_mut()
                    .find(|s| s.id == snapshot_id)
                    .and_then(|s| s.annotations.iter_mut().find(|a| a.id == annotation_id))
                    .ok_or_else(|| {
                        OrbitError::InvalidInput("unknown task quality annotation".into())
                    })?;
                if annotation.dismissal.is_none() {
                    annotation.dismissal = Some(TaskQualityDismissal {
                        reason,
                        at: Utc::now(),
                    });
                }
                ledger::write_quality_history(&path, &history)
            })
        })
    }

    /// Production owner-clock entry. Disabled workspaces return before scans,
    /// queue writes, credential resolution or provider calls.
    pub fn run_task_quality_tick(&self) -> Result<(), OrbitError> {
        self.run_task_quality_with_service(&self.task_quality_service()?)
    }

    /// Supported synthetic-adapter boundary. Reconcile at most 32 subjects and
    /// process at most one bounded attempt per owner tick. A durable rotating
    /// cursor prevents a large workspace from starving its later tasks.
    pub fn run_task_quality_with_service(
        &self,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace_id()?;
        if service
            .assistance_policy_epoch(&workspace, TASK_QUALITY_USE_CASE)?
            .is_none()
        {
            return Ok(());
        }
        self.ensure_coordination_task_write_permitted()?;
        let cursor_path = ledger::quality_ledger_path(self, "reconciliation-cursor")?;
        let ids = ledger::with_quality_history(&cursor_path, || {
            let mut ids: Vec<_> = self
                .list_task_metadata()?
                .into_iter()
                .map(|t| t.id)
                .collect();
            ids.sort();
            let last: String = match std::fs::read(&cursor_path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(ledger::quality_json_error)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e.into()),
            };
            let start = ids.partition_point(|id| id <= &last);
            let count = ids.len().min(32);
            let selected: Vec<_> = ids
                .iter()
                .cycle()
                .skip(start)
                .take(count)
                .cloned()
                .collect();
            if let Some(last) = selected.last() {
                io::atomic_write_text(
                    &cursor_path,
                    &serde_json::to_string(last).map_err(ledger::quality_json_error)?,
                )?;
            }
            Ok(selected)
        })?;
        for id in ids {
            self.schedule_task_quality_with_service(&id, service)?;
            let path = ledger::quality_ledger_path(self, &id)?;
            with_quality_subject(self, &id, || {
                ledger::with_quality_history(&path, || {
                    let task = self.get_task(&id)?;
                    if task.status.is_terminal() {
                        return Ok(());
                    }
                    let mut history = ledger::read_quality_history(&path)?;
                    // Collection starts at first admission, not creation. An owner
                    // restart after the queue-age window can still recover markers.
                    if let Some(snapshot) = history
                        .snapshots
                        .iter_mut()
                        .find(|s| Some(&s.id) == history.current_id.as_ref())
                        && !snapshot.admission_started
                    {
                        snapshot.collected_at = Utc::now();
                        snapshot.admission_started = true;
                        // Persist *before* the service write: an interrupted replay
                        // must hash the same immutable packet and logical work ID.
                        ledger::write_quality_history(&path, &history)?;
                    }
                    let Some(snapshot) = history
                        .snapshots
                        .iter_mut()
                        .find(|s| Some(&s.id) == history.current_id.as_ref())
                    else {
                        return Ok(());
                    };
                    if snapshot.source_digest != contract::task_quality_digest(&task)?
                        || service
                            .assistance_policy_epoch(&workspace, TASK_QUALITY_USE_CASE)?
                            .as_ref()
                            != Some(&snapshot.policy_epoch)
                    {
                        return Ok(());
                    }
                    // Recover a displaced receipt even after a history read
                    // cleared its current projection. Age/retention gaps keep
                    // the immutable receipt and never restart admission.
                    refresh_restored_snapshot(service, snapshot, false)?;
                    if snapshot.work_id.is_some()
                        || snapshot.coverage_gap == Some(AssistanceReason::InvalidRequest)
                    {
                        return Ok(());
                    }
                    let request = match contract::task_quality_request(
                        &task,
                        &workspace,
                        snapshot.collected_at,
                    ) {
                        Ok(request) => request,
                        Err(_) => {
                            snapshot.coverage_gap = Some(AssistanceReason::InvalidRequest);
                            return ledger::write_quality_history(&path, &history);
                        }
                    };
                    let admission = {
                        service.enqueue_expanded_assistance(
                            request,
                            &[],
                            &task_quality_question_template(),
                        )
                    };
                    match admission {
                        Ok(
                            AssistanceAdmission::Enqueued(work)
                            | AssistanceAdmission::Existing(work),
                        ) => {
                            snapshot.work_id = Some(work);
                            snapshot.coverage_gap = None;
                        }
                        Ok(AssistanceAdmission::QueueFull) => {
                            snapshot.coverage_gap = Some(AssistanceReason::QueueFull)
                        }
                        Ok(AssistanceAdmission::Disabled) => return Ok(()),
                        Err(_) => snapshot.coverage_gap = Some(AssistanceReason::Unavailable),
                    }
                    ledger::write_quality_history(&path, &history)
                })
            })?;
            // Copy completed typed results into retained consumer history.
            self.task_quality_history(&id, service)?;
        }
        service.process_next_assistance_checked(&workspace, TASK_QUALITY_USE_CASE, |request| {
            let task = self.get_task(&request.evidence_packet.subject_id)?;
            Ok(!task.status.is_terminal()
                && contract::task_quality_digest(&task)? == request.source_digest)
        })?;
        Ok(())
    }
}
