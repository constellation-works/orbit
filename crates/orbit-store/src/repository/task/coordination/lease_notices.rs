//! Lease notification of task surfaces, including publications without a
//! reservation. A stable surface key makes recovery and index repair replay safe.

use orbit_common::OrbitError;
use orbit_types::task::{TaskEnvelopeV2, TaskStatus, is_no_diff_expected};
use serde_json::json;

use super::TaskCommitBoundary;
use super::admission::canonical_footprint;
use crate::driver::sqlite::task_reservation_store::notices::LeaseOverlapNotice;

pub(super) struct TaskLeaseNotice {
    orbit_dir: String,
    workspace_id: String,
    task_id: String,
    paths: Vec<String>,
    event_key: String,
}

impl TaskLeaseNotice {
    pub(super) fn lease_overlap_event(&self) -> LeaseOverlapNotice<'_> {
        LeaseOverlapNotice {
            orbit_dir: &self.orbit_dir,
            workspace_id: Some(&self.workspace_id),
            holder: &self.task_id,
            paths: &self.paths,
            kind: "overlap",
            payload: json!({"task_id": self.task_id}),
            event_key: &self.event_key,
            landing: None,
        }
    }
}

impl TaskCommitBoundary {
    pub(super) fn task_lease_notice(
        &self,
        envelope: &TaskEnvelopeV2,
    ) -> Result<Option<TaskLeaseNotice>, OrbitError> {
        if !matches!(envelope.status, TaskStatus::InProgress | TaskStatus::Review)
            || is_no_diff_expected(&envelope.tags)
            || envelope.context_files.is_empty()
        {
            return Ok(None);
        }
        let Some(checkout) = self.registry.find_workspace_checkout(&self.workspace_id)? else {
            return Ok(None);
        };
        let paths = canonical_footprint(&envelope.context_files, &checkout.repo_root)?;
        Ok(Some(TaskLeaseNotice {
            orbit_dir: checkout.orbit_dir.to_string_lossy().into_owned(),
            workspace_id: self.workspace_id.clone(),
            task_id: envelope.id.clone(),
            event_key: format!("task:{}:{}", envelope.id, json!(paths)),
            paths,
        }))
    }

    pub(crate) fn record_task_lease_overlaps(
        &self,
        envelope: &TaskEnvelopeV2,
    ) -> Result<(), OrbitError> {
        if let Some(notice) = self.task_lease_notice(envelope)? {
            self.store
                .record_lease_overlaps(&notice.lease_overlap_event())?;
        }
        Ok(())
    }
}
