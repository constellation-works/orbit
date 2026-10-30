//! The visual review contract a delivery captures against (§2.4, §6.2):
//! the owner's `visual_review` settings, the manifests at the base
//! revision, and the mock digests frozen when the delivery was admitted.

use orbit_common::OrbitError;
use orbit_types::task::{
    MOCK_DIGESTS_ARTIFACT, Task, VisualManifestCapture, VisualManifestRef, VisualMockDigest,
    VisualMockDigestRecord, VisualReviewContract, has_verify_mock_ui_tag, mock_sha256_hex,
    record_mock_digests,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::mock_admission::MockDigestFreezeMode;
use crate::application::tag_instructions::read_blob_at;

/// Where a contract's mock digests come from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MockDigestSource {
    /// The digests this delivery froze when it was admitted
    /// (`visual/mock-digests.json`), freezing them now when none exist.
    Frozen,
    /// The mocks' current bytes: a claim admission freezes them into the
    /// receipt it returns.
    Current,
}

impl OrbitRuntime {
    /// The contract for `task`, or `None` when the visual phase does not
    /// apply to it (`verify-mock-ui` only, `visual_review.mode` not `off`).
    /// A mock or manifest that no longer passes admission is an error
    /// naming it, never a silent skip.
    pub(super) fn visual_review_contract_for(
        &self,
        task: &Task,
        source: MockDigestSource,
    ) -> Result<Option<VisualReviewContract>, String> {
        let settings = self.visual_review_settings().clone();
        if !VisualReviewContract::applies_to(&task.tags, settings.mode) {
            return Ok(None);
        }
        let Some(resolved) = self.mock_admission_check(task)? else {
            return Ok(None);
        };
        let artifacts = self
            .get_task_artifacts(&task.id)
            .map_err(|error| format!("could not read task artifacts: {error}"))?;
        let frozen = match source {
            MockDigestSource::Frozen => artifacts
                .iter()
                .find(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
                .map(|artifact| {
                    serde_json::from_slice::<VisualMockDigestRecord>(&artifact.content)
                        .map(|record| record.mocks)
                        .map_err(|error| format!("{MOCK_DIGESTS_ARTIFACT} is unreadable: {error}"))
                })
                .transpose()?,
            MockDigestSource::Current => None,
        };
        let mocks = match frozen {
            Some(mocks) => mocks,
            None => {
                if source == MockDigestSource::Frozen {
                    // A run admitted before digests were frozen on every
                    // delivery path freezes them at its first capture.
                    self.record_admitted_mock_digests(
                        task,
                        None,
                        MockDigestFreezeMode::KeepExisting,
                    )
                    .map_err(|error| format!("could not freeze mock digests: {error}"))?;
                }
                record_mock_digests(&resolved, &artifacts)
                    .mocks
                    .iter()
                    .map(VisualMockDigest::from)
                    .collect()
            }
        };
        let revision = self.mock_base_revision()?;
        let mut manifests = Vec::new();
        for path in &settings.manifests {
            let bytes = read_blob_at(&self.paths().repo_root, &revision, path)
                .map_err(|reason| format!("{path} at {revision}: {reason}"))?;
            let sha256 = mock_sha256_hex(&bytes);
            let content = String::from_utf8(bytes)
                .map_err(|_| format!("{path} at {revision}: manifest is not UTF-8"))?;
            manifests.push(VisualManifestCapture {
                reference: VisualManifestRef {
                    path: path.clone(),
                    revision: revision.clone(),
                    sha256,
                },
                content,
            });
        }
        Ok(Some(VisualReviewContract {
            task_id: task.id.clone(),
            mode: settings.mode,
            default_width: settings.default_width,
            max_mocks: settings.max_mocks,
            reviewer: settings.reviewer,
            max_images: settings.max_images,
            max_image_tokens: settings.max_image_tokens,
            max_findings: settings.max_findings,
            require_established: settings.require_established,
            manifests,
            mocks,
        }))
    }

    /// The contract a claim admission returns in its receipt for `task`.
    pub(crate) fn captured_visual_review(
        &self,
        task: &Task,
    ) -> Result<Option<VisualReviewContract>, OrbitError> {
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(None);
        }
        self.visual_review_contract_for(task, MockDigestSource::Current)
            .map_err(|detail| {
                OrbitError::PolicyDenied(format!(
                    "task '{}' is not admitted: its visual review contract cannot be captured: \
                     {detail}",
                    task.id
                ))
            })
    }

    /// Freeze the mocks of each `verify-mock-ui` task a fresh
    /// `task_pr_pipeline` submission names (§2.4), so a mock replaced
    /// during the delivery is refused as `mock_changed`. Admission's own
    /// mock refusal stays where it is; a task whose mocks do not pass is
    /// not frozen here, and its capture records why.
    pub(crate) fn freeze_visual_admission(
        &self,
        job_name: &str,
        input: &mut Value,
        resuming: bool,
    ) -> Result<(), OrbitError> {
        if job_name != "task_pr_pipeline" || resuming || self.worker_invocation().is_some() {
            return Ok(());
        }
        if self.visual_review_settings().mode == orbit_types::task::VisualReviewMode::Off {
            return Ok(());
        }
        let task_ids = input
            .get("task_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut mock_admissions = input
            .get("__mock_admissions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for task_id in task_ids.iter().filter_map(Value::as_str) {
            let Ok(task) = self.get_task(task_id) else {
                continue;
            };
            if mock_admissions.contains_key(task_id)
                || !matches!(self.mock_admission_check(&task), Ok(Some(_)))
            {
                continue;
            }
            let admission_id = format!(
                "visual:{}",
                mock_sha256_hex(
                    format!(
                        "{task_id}|{}|{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |duration| duration.as_nanos()),
                        std::process::id()
                    )
                    .as_bytes()
                )
            );
            self.record_admitted_mock_digests(
                &task,
                Some(admission_id.clone()),
                Self::mock_freeze_mode_for_ship(&task),
            )?;
            mock_admissions.insert(task_id.to_string(), json!(admission_id));
        }
        if !mock_admissions.is_empty() {
            input["__mock_admissions"] = Value::Object(mock_admissions);
        }
        Ok(())
    }

    /// The contract of the claimed leaf this process executes: what its
    /// owner captured at admission, never this replica's settings. `None`
    /// when the receipt carries none (the phase did not apply, or an older
    /// owner admitted the claim).
    pub(super) fn claimed_visual_review(
        &self,
        task_id: &str,
    ) -> Result<Option<VisualReviewContract>, OrbitError> {
        let leaf = self.current_claimed_leaf()?;
        Ok(leaf
            .admission
            .receipt
            .and_then(|receipt| receipt.visual_review)
            .filter(|contract| contract.task_id == task_id))
    }
}
