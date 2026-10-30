//! `verify-mock-ui` admission: validate a task's mocks against the view
//! manifests at the workspace's current base revision, and freeze mock
//! digests when a delivery is admitted (visual-review §2.3, §2.4).

use orbit_common::OrbitError;
use orbit_types::task::{
    MOCK_DIGESTS_ARTIFACT, MockDigestRecord, ResolvedMock, Task, TaskArtifact, TaskStatus,
    ViewManifestSet, has_verify_mock_ui_tag, mock_refusals_detail,
    record_mock_digests_for_admission, validate_task_mocks,
};

use crate::runtime::run_input::recorded_task_ids;

/// Whether a freeze may replace an existing [`MOCK_DIGESTS_ARTIFACT`]
/// baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MockDigestFreezeMode {
    /// This call committed a new delivery admission, so it establishes that
    /// delivery's baseline even when an earlier delivery left one behind.
    NewDelivery,
    /// This call belongs to a delivery that may already be active (a replay,
    /// a repair claim, or a resubmission of an `in-progress` task), so an
    /// existing baseline is the active delivery's and is kept untouched.
    KeepExisting,
}

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
    ///
    /// The frozen bytes are the exact artifact snapshot validated here, and
    /// the record is keyed to `admission_id`. Under
    /// [`MockDigestFreezeMode::KeepExisting`] an existing baseline wins, so
    /// a replay, repair or resubmission cannot rewrite the active
    /// delivery's baseline; under `NewDelivery` the caller has just
    /// committed a fresh admission and replaces an earlier delivery's
    /// record. A mock validation failure is an error, never a silent pass:
    /// callers freeze only after their admission has committed, and they
    /// hold invalid tasks before that point.
    pub(crate) fn record_admitted_mock_digests(
        &self,
        task: &Task,
        admission_id: Option<String>,
        mode: MockDigestFreezeMode,
    ) -> Result<(), OrbitError> {
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(());
        }
        let artifacts = self.get_task_artifacts(&task.id)?;
        if mode == MockDigestFreezeMode::KeepExisting
            && artifacts
                .iter()
                .any(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
        {
            return Ok(());
        }
        let resolved = self
            .mock_admission_check_with(task, Some(artifacts.clone()))
            .map_err(|detail| {
                OrbitError::InvalidInput(format!(
                    "task '{}' mock digests cannot be frozen: {detail}",
                    task.id
                ))
            })?;
        let Some(resolved) = resolved else {
            return Ok(());
        };
        self.write_mock_digest_record(task, &resolved, &artifacts, admission_id)
    }

    fn write_mock_digest_record(
        &self,
        task: &Task,
        resolved: &[ResolvedMock],
        artifacts: &[TaskArtifact],
        admission_id: Option<String>,
    ) -> Result<(), OrbitError> {
        let artifact = mock_digest_artifact(resolved, artifacts, admission_id)?;
        self.update_task_as_system(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![artifact],
                ..Default::default()
            },
            None,
        )?;
        Ok(())
    }

    /// The frozen digest artifact a pull admission publishes in the same
    /// commit as its claim, or `None` when the task is not
    /// `verify-mock-ui` or an active delivery's baseline must stand.
    ///
    /// An ordinary task write is refused once the claim protects the task,
    /// so the owner cannot freeze after the boundary commits; the artifact
    /// rides the claim commit instead (see
    /// `AdmissionTaskGuidance::artifacts`). A `backlog` task is a new
    /// delivery and (re)freezes; an `in-progress` task (a repair claim)
    /// keeps its existing baseline and freezes only when none exists.
    pub(crate) fn mock_digest_artifact_for_pull_admission(
        &self,
        task: &Task,
        request_id: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(None);
        }
        let artifacts = self.get_task_artifacts(&task.id)?;
        if task.status == TaskStatus::InProgress
            && artifacts
                .iter()
                .any(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
        {
            return Ok(None);
        }
        let resolved = self
            .mock_admission_check_with(task, Some(artifacts.clone()))
            .map_err(|detail| {
                OrbitError::InvalidInput(format!(
                    "task '{}' mock digests cannot be frozen: {detail}",
                    task.id
                ))
            })?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        Ok(Some(mock_digest_artifact(
            &resolved,
            &artifacts,
            Some(format!("claim-request:{request_id}")),
        )?))
    }

    /// Freeze at the workflow-admission transition
    /// (`admit_task_for_workflow_as_system`): the shared start of explicit
    /// ship, automatic backlog discovery and local-drain leaves.
    ///
    /// A task this call moved `backlog -> in-progress` starts a new
    /// delivery, so its baseline is (re)frozen — unless a still-live
    /// delivery run already froze it for this same delivery (an explicit
    /// ship freezes at submission, keyed to its run), in which case that
    /// record is this delivery's and is kept. A task that was already
    /// `in-progress` is the active delivery's own retry: its existing
    /// baseline is kept, and one is written only when none exists yet.
    pub(crate) fn freeze_mock_digests_at_workflow_admission(
        &self,
        task: &Task,
        transitioned: bool,
    ) -> Result<(), OrbitError> {
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(());
        }
        if !transitioned {
            return self.record_admitted_mock_digests(
                task,
                self.live_delivery_admission_id(&task.id),
                MockDigestFreezeMode::KeepExisting,
            );
        }
        let live = self.live_delivery_run_ids_for_task(&task.id)?;
        if let Some(existing) = self.existing_mock_digest_record(task)? {
            let frozen_by_live_run = existing.admission_id.as_deref().is_some_and(|id| {
                live.iter().any(|run_id| id == format!("run:{run_id}"))
                    || self
                        .live_mock_admission_tokens_for_task(&task.id)
                        .is_ok_and(|tokens| tokens.iter().any(|token| token == id))
            });
            if frozen_by_live_run {
                return Ok(());
            }
        }
        let admission_id = live.first().map(|run_id| format!("run:{run_id}"));
        self.record_admitted_mock_digests(task, admission_id, MockDigestFreezeMode::NewDelivery)
    }

    fn existing_mock_digest_record(
        &self,
        task: &Task,
    ) -> Result<Option<MockDigestRecord>, OrbitError> {
        let artifacts = self.get_task_artifacts(&task.id)?;
        let Some(artifact) = artifacts
            .iter()
            .find(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
        else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&artifact.content).ok())
    }

    fn live_delivery_admission_id(&self, task_id: &str) -> Option<String> {
        self.live_delivery_run_ids_for_task(task_id)
            .ok()
            .and_then(|runs| runs.into_iter().next())
            .map(|run_id| format!("run:{run_id}"))
    }

    /// Non-terminal delivery runs carrying `task_id`, newest first as
    /// `list_job_runs` returns them (the same holders the ship in-flight
    /// guard keys on).
    fn live_delivery_runs_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::workflow::JobRun>, OrbitError> {
        let delivery_jobs = self.task_delivery_job_ids()?;
        let runs = self.list_job_runs(crate::application::job::JobRunListParams {
            limit: Some(200),
            ..Default::default()
        })?;
        Ok(runs
            .into_iter()
            .filter(|run| !run.state.is_terminal() && delivery_jobs.contains(&run.job_id))
            .filter(|run| {
                run.input
                    .as_ref()
                    .map(recorded_task_ids)
                    .into_iter()
                    .flatten()
                    .any(|candidate| candidate == task_id)
            })
            .collect())
    }

    fn live_delivery_run_ids_for_task(&self, task_id: &str) -> Result<Vec<String>, OrbitError> {
        Ok(self
            .live_delivery_runs_for_task(task_id)?
            .into_iter()
            .map(|run| run.run_id)
            .collect())
    }

    /// Mock-admission tokens (`__mock_admissions`) that live delivery runs
    /// carrying `task_id` were submitted with: an explicit ship freezes
    /// before its run exists, so its token in the run input is how the
    /// leaf recognises that freeze as this delivery's.
    fn live_mock_admission_tokens_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<String>, OrbitError> {
        Ok(self
            .live_delivery_runs_for_task(task_id)?
            .iter()
            .filter_map(|run| run.input.as_ref())
            .filter_map(|input| input.get("__mock_admissions"))
            .filter_map(|admissions| admissions.get(task_id))
            .filter_map(|token| token.as_str())
            .map(ToOwned::to_owned)
            .collect())
    }

    /// The freeze mode for an explicit ship of `task`: a `backlog` task's
    /// successfully submitted run is a new delivery; an `in-progress`
    /// task already has an active delivery whose baseline must stand.
    pub(crate) fn mock_freeze_mode_for_ship(task: &Task) -> MockDigestFreezeMode {
        if task.status == TaskStatus::InProgress {
            MockDigestFreezeMode::KeepExisting
        } else {
            MockDigestFreezeMode::NewDelivery
        }
    }
}

/// The [`MOCK_DIGESTS_ARTIFACT`] payload freezing `resolved` against the
/// exact `artifacts` bytes validated, keyed to `admission_id`.
fn mock_digest_artifact(
    resolved: &[ResolvedMock],
    artifacts: &[TaskArtifact],
    admission_id: Option<String>,
) -> Result<TaskArtifact, OrbitError> {
    let record = record_mock_digests_for_admission(resolved, artifacts, admission_id);
    let content = serde_json::to_vec_pretty(&record)
        .map_err(|error| OrbitError::Execution(format!("encode mock digests: {error}")))?;
    Ok(TaskArtifact {
        path: MOCK_DIGESTS_ARTIFACT.to_string(),
        content,
        media_type: "application/json".to_string(),
        created_by: None,
    })
}
