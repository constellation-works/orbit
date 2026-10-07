//! A claimed leaf's host runs the `host_sandbox_test` checks its reviewer
//! could not run inside the agent sandbox [ORB-14478].
//!
//! Every agent lane runs inside Orbit's own sandbox, and the kernel refuses a
//! nested one: a macOS `sandbox_apply` fails with `EPERM`, and a Linux
//! Bubblewrap cannot create its namespaces. Tests of Orbit's sandbox paths
//! therefore skip or fail inside the reviewer, which then reports
//! `incomplete` and names each such check as a `host_sandbox_test`
//! requirement with the host OS it needs. Settlement on the claimed leaf's
//! own host, Orbit's deterministic worker, runs each one whose OS is this
//! host's, before the verdict is reconciled:
//!
//! - only when the report is otherwise evidence-only, so a review that would
//!   block anyway runs nothing;
//! - only a command [`HostSandboxCommand::admit`] admits — the `cargo test`
//!   shape or an owner-required command — refused with a typed reason and
//!   never run otherwise;
//! - at the final candidate, in a fresh detached worktree, outside the agent
//!   sandbox ([`run_host_sandbox_test`]).
//!
//! Each run attaches its log; a passing one also attaches its result, which
//! settlement counts like any external evidence on this tree, so the review
//! can pass without a hold. A run that skipped, deferred, ran no test or met
//! an unavailable sandbox leaves the evidence missing, and the review holds.
//! A failed run fails the reviewer's record, and the review blocks. The
//! certificate keeps every record, and a passed verdict's handoff pins each
//! result and log for the owner to check.

use std::collections::BTreeMap;

use orbit_automation::review::{ValidationContext, validation_evidence};
use orbit_common::OrbitError;
use orbit_engine::review_gate::run_host_sandbox_test;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    EvidenceHostOs, FindingDisposition, HostEvidenceReason, HostEvidenceRecord, HostSandboxCommand,
    ReviewEvidenceKind, ReviewExternalEvidence, ValidationOutcome, ValidationRole,
};
use serde_json::json;

use crate::OrbitRuntime;

use super::super::evidence::{
    canonical_requirements, satisfied_external_evidence, with_external_checks_passed,
};
use super::super::fulfilment::fulfilment_artifact_paths;
use super::context::GateContext;
use super::judgement::{Judgement, write_artifact};

impl Judgement {
    /// Run each `host_sandbox_test` requirement this claimed leaf's host can
    /// run, and return the passing results by artifact path for settlement
    /// to count on `candidate`.
    pub(super) fn fulfil_host_evidence(
        &mut self,
        runtime: &OrbitRuntime,
        context: &GateContext,
        attempt_id: &str,
        candidate: &SourceRevision,
        scope: &[String],
    ) -> Result<BTreeMap<String, ReviewExternalEvidence>, OrbitError> {
        let mut satisfied = BTreeMap::new();
        let [task_id] = context.task_ids.as_slice() else {
            return Ok(satisfied);
        };
        if !context.claimed
            || self.verdict.passed()
            || self
                .findings
                .iter()
                .any(|finding| finding.disposition == FindingDisposition::Open)
            || !self
                .external_evidence
                .iter()
                .any(|required| required.kind == ReviewEvidenceKind::HostSandboxTest)
        {
            return Ok(satisfied);
        }
        // Run nothing for a review that would not hold for its evidence.
        let Some(requirements) = canonical_requirements(&self.external_evidence) else {
            return Ok(satisfied);
        };
        let Some(validation) = with_external_checks_passed(&self.validation, &requirements) else {
            return Ok(satisfied);
        };
        if validation_evidence(
            &validation,
            &ValidationContext {
                scope,
                obligations: &self.retained_obligations,
                retired: &self.retired_validation,
                required_validation_commands: self.required_validation_commands.as_deref(),
            },
        )
        .is_err()
        {
            return Ok(satisfied);
        }
        let arrived = satisfied_external_evidence(runtime, task_id, candidate)?;
        let host_os = EvidenceHostOs::of_host(runtime.host_os());
        let owner_required = self
            .required_validation_commands
            .clone()
            .unwrap_or_default();
        for required in requirements
            .iter()
            .filter(|required| required.kind == ReviewEvidenceKind::HostSandboxTest)
        {
            if arrived
                .values()
                .any(|evidence| evidence.matches_requirement(required, candidate))
            {
                continue;
            }
            let Some(os) = required.os else {
                continue;
            };
            let mut record = HostEvidenceRecord {
                name: required.name.clone(),
                command: required.command.clone(),
                host_command: None,
                os,
                tree: candidate.tree.clone(),
                passed: false,
                reason: None,
                detail: String::new(),
                artifact: None,
                log_artifact: None,
            };
            let admitted = if host_os == Some(os) {
                HostSandboxCommand::admit(&required.command, &owner_required)
            } else {
                Err(orbit_types::workflow::HostEvidenceRefusal {
                    reason: HostEvidenceReason::OsMismatch,
                    detail: format!(
                        "the requirement needs {}; this host is {}",
                        os.as_str(),
                        runtime
                            .host_os()
                            .map_or("an unknown OS", orbit_types::task::HostOs::as_str)
                    ),
                })
            };
            let command = match admitted {
                Ok(command) => command,
                Err(refusal) => {
                    record.reason = Some(refusal.reason);
                    record.detail = refusal.detail;
                    self.record_host_refusal(&record);
                    self.host_evidence.push(record);
                    continue;
                }
            };
            let (artifact, log_artifact) = fulfilment_artifact_paths(&required.artifact)?;
            let run = run_host_sandbox_test(
                runtime,
                &context.workspace_path,
                candidate,
                &command,
                &context.run_id,
            )?;
            record.host_command = (!run.host_command.is_empty()).then(|| run.host_command.clone());
            let log = json!({
                "schema_version": 1,
                "run_id": context.run_id,
                "task_id": task_id,
                "attempt_id": attempt_id,
                "tested_head": candidate.commit,
                "tested_tree": candidate.tree,
                "kind": ReviewEvidenceKind::HostSandboxTest,
                "os": os,
                "command": required.command,
                "host_command": run.host_command,
                "exit_code": run.exit_code,
                "timed_out": run.timed_out,
                "outcome": if run.judgement.is_ok() { "passed" } else { "failed" },
                "reason": run.judgement.as_ref().err().map(|refusal| refusal.reason),
                "detail": run.judgement.as_ref().err().map(|refusal| refusal.detail.as_str()),
                "tests_passed": run.judgement.as_ref().ok(),
                "output": run.output,
                "validation_env": run.environment,
                "recorded_at": chrono::Utc::now().to_rfc3339(),
            });
            write_artifact(
                runtime,
                task_id,
                &context.run_id,
                &log_artifact,
                &serde_json::to_vec_pretty(&log).map_err(|error| {
                    OrbitError::Execution(format!("serialize {log_artifact}: {error}"))
                })?,
            )?;
            record.log_artifact = Some(log_artifact.clone());
            match run.judgement {
                Ok(_) => {
                    let evidence = ReviewExternalEvidence {
                        schema_version: 1,
                        attempt_id: attempt_id.to_string(),
                        candidate: candidate.clone(),
                        kind: ReviewEvidenceKind::HostSandboxTest,
                        name: required.name.clone(),
                        command: required.command.clone(),
                        outcome: ValidationOutcome::Passed,
                        log_artifact,
                        os: Some(os),
                    };
                    write_artifact(
                        runtime,
                        task_id,
                        &context.run_id,
                        &artifact,
                        &serde_json::to_vec_pretty(&evidence).map_err(|error| {
                            OrbitError::Execution(format!("serialize {artifact}: {error}"))
                        })?,
                    )?;
                    record.passed = true;
                    record.artifact = Some(artifact.clone());
                    satisfied.insert(artifact, evidence);
                }
                Err(refusal) => {
                    record.reason = Some(refusal.reason);
                    record.detail = refusal.detail;
                    if refusal.reason == HostEvidenceReason::TestFailed {
                        for validation in &mut self.validation {
                            if validation.command == required.command
                                && validation.role == ValidationRole::Required
                            {
                                validation.outcome = ValidationOutcome::Failed;
                                validation.note = Some(format!(
                                    "The host ran it outside the agent sandbox and it failed; \
                                     log {}",
                                    record.log_artifact.as_deref().unwrap_or_default()
                                ));
                            }
                        }
                    }
                    self.record_host_refusal(&record);
                }
            }
            self.host_evidence.push(record);
        }
        Ok(satisfied)
    }

    fn record_host_refusal(&mut self, record: &HostEvidenceRecord) {
        let reason = record.reason.map_or("unknown", HostEvidenceReason::as_str);
        self.escalate(&format!(
            "host_sandbox_test_{reason}: `{}` ({}) on {}: {}",
            record.command,
            record.name,
            record.os.as_str(),
            record.detail
        ));
    }
}
