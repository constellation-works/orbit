//! Artifact and validation evidence for an observed handoff.
use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::{KnownRedValidation, handoff::*};

use super::super::lifecycle::invalid;
use super::review::known_red_validation;
use super::{TaskCommitBoundary, object_id};
use crate::contracts::*;

impl TaskCommitBoundary {
    pub(super) fn artifact_bytes(
        &self,
        task_id: &str,
        reference: &HandoffArtifactRef,
    ) -> Result<Vec<u8>, OrbitError> {
        orbit_types::task::validate_relative_artifact_path(&reference.path)?;
        // Full bundle read verifies the manifest and payload digests before exposure.
        let bundle = self.bundle_store.read_bundle(task_id)?;
        let file = bundle
            .artifact_manifest
            .as_ref()
            .and_then(|m| m.files.iter().find(|f| f.path == reference.path))
            .ok_or_else(|| invalid("owner validation artifact missing"))?;
        if file.sha256 != reference.sha256 {
            return Err(invalid("validation artifact changed"));
        }
        let bytes = std::fs::read(
            self.bundle_store
                .bundle_path(task_id)?
                .join(orbit_types::task::TASK_ARTIFACTS_DIR_NAME)
                .join(&file.blob),
        )?;
        if sha256_hex(&bytes) != reference.sha256 {
            return Err(invalid("validation artifact changed"));
        }
        Ok(bytes)
    }

    /// Check the validation logs a handoff pinned against the owner's
    /// required commands, and return the known-red commands among them
    /// [ORB-15427]. A `no-diff-expected` NoDiff skip on a task the owner
    /// still tags needs no log for a required command [ORB-15764].
    ///
    /// A log passes with exit status 0, or fails only with tests its base
    /// fails too: a non-zero log must carry the leaf's known-red report for
    /// the handed-off base. Acceptance passes `known_red`, the owner's own
    /// base runs, and accepts such a log only when the owner's run shares
    /// exactly the failures it reports; approval and landing pass `None` and
    /// recheck the pinned logs, which acceptance already judged.
    pub(super) fn validate_handoff_evidence(
        &self,
        handoff: &TaskHandoff,
        required: &[String],
        known_red: Option<&[KnownRedObservation]>,
    ) -> Result<Vec<KnownRedValidation>, OrbitError> {
        // An empty requirement list is no required check; any log the
        // handoff still carries must pin this claim's candidate all the same.
        let commands: BTreeSet<_> = required.iter().map(String::as_str).collect();
        if commands.iter().any(|c| c.trim().is_empty()) || commands.len() != required.len() {
            return Err(invalid("owner validation requirements blank or duplicated"));
        }
        let mut passed = BTreeSet::new();
        let mut known = Vec::new();
        for reference in &handoff.validation {
            let log: HandoffValidationLog =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                    .map_err(|e| invalid(&format!("invalid validation log: {e}")))?;
            let report = match (&log.known_red, log.exit_code) {
                (None, 0) => None,
                (Some(report), code) if code != 0 => Some(report),
                _ => {
                    return Err(invalid(
                        "validation must pass for the exact claim/run/candidate/base",
                    ));
                }
            };
            if log.schema_version != 1
                || log.workspace_id != handoff.workspace_id
                || log.task_id != handoff.task_id
                || log.claim_id != handoff.claim_id
                || log.machine_id != handoff.machine_id
                || log.run_id != handoff.run_id
                || log.tested_head != handoff.candidate.candidate.commit
                || log.candidate != handoff.candidate
                || log.command.trim().is_empty()
                || !passed.insert(log.command.clone())
            {
                return Err(invalid(
                    "validation must pass for the exact claim/run/candidate/base",
                ));
            }
            if let Some(flaky) = &log.flaky
                && (!flaky.valid()
                    || report.is_some()
                    || log.exit_code != 0
                    || flaky.command != log.command
                    || flaky.run_id != log.run_id
                    || flaky.base_sha != log.candidate.base.commit
                    || flaky.tested_head != log.tested_head
                    || flaky.rerun_output != log.output)
            {
                return Err(invalid(
                    "flaky validation evidence does not match the exact handoff",
                ));
            }
            if let Some(report) = report {
                known.push(known_red_validation(&log, report, handoff, known_red)?);
            }
        }
        let mut no_diff_expected = false;
        if let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery {
            let report: serde_json::Value =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, evidence)?)
                    .map_err(|e| invalid(&format!("invalid NoDiff verifier report: {e}")))?;
            if handoff.candidate.candidate != handoff.candidate.base
                || !handoff.footprint_widening.is_empty()
                || report["task_id"] != handoff.task_id
                || report["job_run_id"] != handoff.run_id
                || report["base_sha"] != handoff.candidate.base.commit
                || !matches!(
                    report["decision"].as_str(),
                    Some(
                        "verified_no_diff" | "verified_already_landed" | "skipped_no_diff_expected"
                    )
                )
            {
                return Err(invalid("NoDiff verifier report identity or base mismatch"));
            }
            // [ORB-14791] A claimed `no-diff-expected` skip carries no report
            // of its own: the owner's copy of the task must still be tagged.
            no_diff_expected = report["decision"] == "skipped_no_diff_expected";
            if no_diff_expected
                && !self
                    .bundle_store
                    .read_bundle_lightweight(&handoff.task_id)?
                    .envelope
                    .tags
                    .iter()
                    .any(|tag| tag == orbit_types::task::NO_DIFF_EXPECTED_TAG)
            {
                return Err(invalid("NoDiff skip requires a no-diff-expected task"));
            }
        }
        // [ORB-15764] Only that tagged skip is exempt: its leaf runs no
        // required command, as the owner's own pipelines skip `validate` for
        // the tag. Any log it still carries, such as one from a leaf built
        // before the exemption, was checked above like every other.
        if !no_diff_expected && !commands.iter().all(|c| passed.contains(*c)) {
            return Err(invalid("required validation missing"));
        }
        if let HandoffDelivery::AlreadyLanded {
            covering_commit,
            evidence,
        } = &handoff.candidate.delivery
        {
            let proof: AlreadyLandedEvidence =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, evidence)?)
                    .map_err(|e| invalid(&format!("invalid already-landed evidence: {e}")))?;
            let bundle = self
                .bundle_store
                .read_bundle_lightweight(&handoff.task_id)?;
            let comments = bundle
                .comments
                .iter()
                .cloned()
                .map(crate::repository::task::v2::task_comment_from_row)
                .collect::<Vec<_>>();
            let task = crate::repository::task::TaskV2Store::new(
                self.registry.clone(),
                self.workspace_id.clone(),
            )
            .task_from_bundle(bundle)?;
            if proof.schema_version != 1
                || proof.task_id != handoff.task_id
                || proof.covering_task_id != handoff.task_id
                || proof.run_id != handoff.run_id
                || proof.tested_head != handoff.candidate.candidate.commit
                || proof.covering_commit != *covering_commit
                || !object_id(covering_commit)
                || handoff.candidate.candidate != handoff.candidate.base
                || proof.scope != already_landed_scope(&task, &comments)
                || proof.required_commands != required
                || proof.validation.len() != required.len()
                || task.acceptance_criteria.is_empty()
                || proof.criteria_evidence.len() != task.acceptance_criteria.len()
                || proof.criteria_evidence.iter().any(|s| s.trim().is_empty())
            {
                return Err(invalid(
                    "already-landed evidence identity, scope or criteria mismatch",
                ));
            }
            let mut checked = BTreeSet::new();
            for check in &proof.validation {
                if check.validation.outcome != orbit_types::workflow::ValidationOutcome::Passed
                    || check.validation.role != orbit_types::workflow::ValidationRole::Required
                    || !commands.contains(check.validation.command.as_str())
                    || !checked.insert(&check.validation.command)
                {
                    return Err(invalid("already-landed validation incomplete"));
                }
                let reference = handoff
                    .validation
                    .iter()
                    .find(|r| r.path == check.log_artifact)
                    .ok_or_else(|| invalid("already-landed validation log missing"))?;
                let log: HandoffValidationLog =
                    serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                        .map_err(|e| invalid(&format!("invalid validation log: {e}")))?;
                if log.command != check.validation.command || log.exit_code != 0 {
                    return Err(invalid("already-landed validation log mismatch"));
                }
            }
        }
        Ok(known)
    }

    pub(super) fn observe_handoff<'a>(
        &self,
        auth: &'a ClaimInvocation,
        candidate: &HandoffCandidate,
    ) -> Result<&'a HandoffObservation, OrbitError> {
        let observation = auth
            .handoff_observation
            .as_ref()
            .ok_or_else(|| invalid("trusted owner candidate observation required"))?;
        if observation.candidate != *candidate {
            return Err(invalid("candidate or delivery identity changed"));
        }
        Ok(observation)
    }
}
