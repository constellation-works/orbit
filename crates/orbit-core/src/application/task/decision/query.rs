//! Read a task's effective decisions from one bundle snapshot.

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestFileV2, DecisionRequest, Task, TaskComment, TaskHistoryEntry, TaskStatus,
};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Read effective human-decision requests, rechecking the current task,
    /// block episode and pilot admission hold without changing task records.
    pub fn get_task_decision_requests(&self, id: &str) -> Result<Vec<DecisionRequest>, OrbitError> {
        // Owner-routed reads retain their authority and transport contract.
        if self.worker_invocation().is_some() {
            let task = self.get_task(id)?;
            let history = self.get_task_history(id)?;
            let comments = self.get_task_comments(id)?;
            return self.task_decision_requests_in(&task, &history, &comments, chrono::Utc::now());
        }
        let stamps = self.task_bundle_stamps(&[id.to_string()]);
        let (requests, observed_at) = self.get_task_decision_requests_for_scan(id)?;
        if !requests.is_empty() || stamps.contains_key(id) {
            self.record_decision_snapshots(&[orbit_store::contracts::TaskDecisionSnapshot {
                task_id: id.to_string(),
                // An unstampable observation can still index the owner;
                // an empty stamp never matches a real bundle proof.
                stamp: stamps.get(id).cloned().unwrap_or_default(),
                projection_key: self.decision_projection_key(),
                observed_at,
                requests: requests.clone(),
            }]);
        }
        Ok(requests)
    }

    /// One lightweight bundle read supplies the current task, history and
    /// comments together. Worker reads stay on the owner's existing route.
    pub(crate) fn get_task_decision_requests_for_scan(
        &self,
        id: &str,
    ) -> Result<(Vec<DecisionRequest>, chrono::DateTime<chrono::Utc>), OrbitError> {
        if self.worker_invocation().is_some() {
            return Ok((self.get_task_decision_requests(id)?, chrono::Utc::now()));
        }
        // Keep the row and its immutable payload checks in one task read
        // section, so an ordinary bundle rewrite cannot interleave them.
        self.in_task_read_section(|| {
            let row = self
                .stores()
                .tasks()
                .get_task_row(id, true)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))?;
            // A lightweight row deliberately defers payload validation. Decisions
            // historically warn for any corrupt blob, including unrelated evidence,
            // so authenticate its manifest entries without reopening the bundle.
            for file in &row.artifacts {
                self.stores()
                    .task_artifacts()
                    .task_artifact_from_manifest(id, file)?;
            }
            let observed_at = chrono::Utc::now();
            let requests = self.task_decision_requests_with_audit(
                &row.task,
                &row.history,
                &row.comments,
                observed_at,
                Some(&row.artifacts),
                &|receipt| {
                    self.task_pilot_audit_with_reader(id, receipt, &|path| match row
                        .artifacts
                        .iter()
                        .find(|file| file.path == path)
                    {
                        Some(file) => self
                            .stores()
                            .task_artifacts()
                            .task_artifact_from_manifest(id, file),
                        None => Ok(None),
                    })
                },
            )?;
            Ok((requests, observed_at))
        })
    }

    /// Effective decision requests over records a read-only aggregate already loaded.
    /// `task` must include its requirements: revision-bound requests cannot be
    /// checked against bodyless task-index metadata.
    pub(crate) fn task_decision_requests_in(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
        comments: &[TaskComment],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<DecisionRequest>, OrbitError> {
        self.task_decision_requests_with_audit(task, history, comments, now, None, &|receipt| {
            self.task_pilot_audit(&task.id, receipt)
        })
    }

    fn task_decision_requests_with_audit(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
        comments: &[TaskComment],
        now: chrono::DateTime<chrono::Utc>,
        manifest: Option<&[ArtifactManifestFileV2]>,
        audit: &dyn Fn(
            orbit_types::task::CommentReceipt<'_>,
        ) -> Result<serde_json::Value, OrbitError>,
    ) -> Result<Vec<DecisionRequest>, OrbitError> {
        let pilot_operation = if task.status == TaskStatus::Backlog
            && self
                .pilot_admission_hold_with_audit(task, comments, &|| Ok(history.to_vec()), audit)?
                .is_some()
        {
            super::super::pilot::latest_pilot_operation_id(comments)
        } else {
            None
        };
        let mut requests =
            orbit_types::task::decision_requests_from_history(task, history, pilot_operation, now);
        self.add_handoff_landing_answers(task, &mut requests, manifest)?;
        Ok(requests)
    }
}
