//! `verify-mock-ui` admission: validate a task's mocks against the view
//! manifests at the workspace's current base revision, and freeze mock
//! digests when a delivery is admitted (visual-review §2.3, §2.4).

use orbit_common::OrbitError;
use orbit_types::task::{
    MOCK_DIGESTS_ARTIFACT, ResolvedMock, Task, TaskArtifact, ViewManifestSet,
    has_verify_mock_ui_tag, mock_refusals_detail, record_mock_digests, validate_task_mocks,
};

use super::tag_instructions::read_blob_at;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

impl OrbitRuntime {
    /// The workspace's current base commit: base branch as last fetched,
    /// else the local branch, else `HEAD` (as experiment overlays read it).
    fn mock_base_revision(&self) -> Result<String, String> {
        let root = &self.paths().repo_root;
        let branch = self.workspace_base_branch();
        let branch = branch.strip_prefix("origin/").unwrap_or(branch);
        crate::application::tag_instructions::commit_at(
            root,
            &format!("refs/remotes/origin/{branch}"),
        )
        .or_else(|_| {
            crate::application::tag_instructions::commit_at(root, &format!("refs/heads/{branch}"))
        })
        .or_else(|_| crate::application::tag_instructions::commit_at(root, "HEAD"))
    }

    /// Configured manifests parsed at the base revision. A manifest that
    /// cannot be read or parsed is an admission refusal naming it, never
    /// a silent pass.
    fn view_manifests_at_base(&self) -> Result<ViewManifestSet, String> {
        let settings = self.visual_review_settings();
        let mut set = ViewManifestSet::default();
        if settings.manifests.is_empty() {
            return Ok(set);
        }
        let revision = self.mock_base_revision()?;
        for path in &settings.manifests {
            let bytes = read_blob_at(&self.paths().repo_root, &revision, path)
                .map_err(|reason| format!("{path} at {revision}: {reason}"))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("{path} at {revision}: manifest is not UTF-8"))?;
            set.add_toml(path, &text)?;
        }
        Ok(set)
    }

    /// Validate `task`'s mocks for admission. `Ok(None)` when the task is
    /// not `verify-mock-ui`; `Ok(Some(resolved))` when its mocks pass;
    /// `Err(detail)` naming every failing artifact when they do not. A
    /// store or manifest read failure is returned as a refusal detail so
    /// admission withholds the task instead of starting it unchecked.
    pub(crate) fn mock_admission_check(
        &self,
        task: &Task,
    ) -> Result<Option<Vec<ResolvedMock>>, String> {
        self.mock_admission_check_with(task, None)
    }

    /// [`Self::mock_admission_check`] over caller-supplied artifacts
    /// instead of the stored ones (a promotion that attaches mocks in
    /// the same update).
    pub(crate) fn mock_admission_check_with(
        &self,
        task: &Task,
        artifacts: Option<Vec<TaskArtifact>>,
    ) -> Result<Option<Vec<ResolvedMock>>, String> {
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(None);
        }
        let settings = self.visual_review_settings().clone();
        let artifacts = match artifacts {
            Some(artifacts) => artifacts,
            None => self
                .get_task_artifacts(&task.id)
                .map_err(|error| format!("could not read task artifacts: {error}"))?,
        };
        let manifests = self.view_manifests_at_base()?;
        match validate_task_mocks(
            &task.tags,
            &artifacts,
            &manifests,
            !settings.manifests.is_empty(),
            settings.default_width,
        ) {
            Ok(resolved) => Ok(Some(resolved)),
            Err(refusals) => Err(mock_refusals_detail(&refusals)),
        }
    }

    /// Refuse a promotion to backlog whose `verify-mock-ui` mocks fail
    /// admission. `artifacts: None` judges the task's stored artifacts;
    /// `Some` judges the artifacts the promoting write would leave stored.
    pub(crate) fn ensure_backlog_mock_admission(
        &self,
        task: &Task,
        artifacts: Option<Vec<TaskArtifact>>,
    ) -> Result<(), OrbitError> {
        self.mock_admission_check_with(task, artifacts)
            .map(|_| ())
            .map_err(|detail| {
                OrbitError::InvalidInput(format!(
                    "task '{}' cannot move to backlog: {detail}",
                    task.id
                ))
            })
    }

    /// Freeze the admitted mocks' digests as the system artifact
    /// [`MOCK_DIGESTS_ARTIFACT`], so a mock replaced mid-delivery is
    /// detectable against this record (`mock_changed`, design §4.4).
    pub(crate) fn record_admitted_mock_digests(
        &self,
        task: &Task,
        resolved: &[ResolvedMock],
    ) -> Result<(), OrbitError> {
        let artifacts = self.get_task_artifacts(&task.id)?;
        let record = record_mock_digests(resolved, &artifacts);
        let content = serde_json::to_vec_pretty(&record)
            .map_err(|error| OrbitError::Execution(format!("encode mock digests: {error}")))?;
        self.update_task_as_system(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: MOCK_DIGESTS_ARTIFACT.to_string(),
                    content,
                    media_type: "application/json".to_string(),
                    created_by: None,
                }],
                ..Default::default()
            },
            None,
        )?;
        Ok(())
    }
}
