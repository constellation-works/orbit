//! Running and attaching one fulfilment at the held commit.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::run_host_sandbox_test;
use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    EvidenceHostOs, HostSandboxCommand, ReviewEvidenceHold, ReviewEvidenceKind,
    ReviewEvidenceRequirement, ReviewExternalEvidence, ValidationOutcome,
};
use serde_json::{Value, json};

use super::super::evidence::evidence_artifact_path;
use super::codeql::admitted_codeql_args;
use crate::OrbitRuntime;
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskUpdateParams, recovery_checkout_path};

use super::tick::{free_mib, fulfilable_hold, hold_key, min_free_mib_of};
use super::{
    DEFAULT_FULFILMENT_MIN_FREE_MIB, EVIDENCE_FULFILMENT_AUDIT, EvidenceRun, FulfilmentRefusal,
    HOLD_KEY_FIELD,
};

/// A requirement's command as fulfilment admitted it.
enum AdmittedCommand {
    /// The CodeQL script's arguments.
    Codeql(Vec<String>),
    HostSandboxTest(HostSandboxCommand),
}

impl OrbitRuntime {
    /// The fulfilment step: re-check the hold, admit every command, gate on
    /// host and disk, run each named command at the held commit, and attach
    /// the logs — plus the results, when every run passed. Always audited.
    pub(crate) fn fulfil_review_evidence(
        &self,
        input: &Value,
        run_id: &str,
    ) -> Result<Value, OrbitError> {
        let task_id = input
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| OrbitError::InvalidInput("`task_id` is required".into()))?;
        let key = input
            .get(HOLD_KEY_FIELD)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let min_free_mib = min_free_mib_of(input).unwrap_or(DEFAULT_FULFILMENT_MIN_FREE_MIB);
        let task = self.get_task(task_id)?;
        let hold = fulfilable_hold(self, &task)?.filter(|hold| hold_key(hold) == key);
        let outcome = match &hold {
            None => Err((FulfilmentRefusal::HoldNotCurrent, String::new())),
            Some(hold) => self.fulfil_hold(&task, hold, run_id, min_free_mib),
        };
        let free_mib = free_mib(&self.paths().state_dir).ok();
        let (refusal, detail, runs) = match outcome {
            Ok(runs) => (
                runs.iter().find_map(|run| run.refusal),
                runs.iter()
                    .find(|run| run.refusal.is_some())
                    .map(|run| run.detail.clone())
                    .unwrap_or_default(),
                runs,
            ),
            Err((refusal, detail)) => (Some(refusal), detail, Vec::new()),
        };
        let mut requeued = false;
        if let Some(hold) = &hold
            && refusal != Some(FulfilmentRefusal::HoldNotCurrent)
        {
            self.attach_fulfilment(&task, hold, run_id, &runs, refusal, &detail)?;
            requeued = self.get_task(task_id)?.status == TaskStatus::Backlog;
        }
        let output = json!({
            "task_id": task_id,
            HOLD_KEY_FIELD: key,
            "fulfilled": refusal.is_none(),
            "reason": refusal.map(FulfilmentRefusal::as_str),
            "detail": detail,
            "retryable": refusal.is_some_and(FulfilmentRefusal::retryable),
            "requeued": requeued,
        });
        self.record_pipeline_audit(
            EVIDENCE_FULFILMENT_AUDIT,
            Some(run_id),
            Some(SYSTEM_ACTOR_LABEL),
            match refusal {
                None => AuditEventStatus::Success,
                Some(FulfilmentRefusal::HoldNotCurrent) => AuditEventStatus::Denied,
                Some(_) => AuditEventStatus::Failure,
            },
            json!({
                "run_id": run_id,
                "task_id": task_id,
                "attempt_id": hold.as_ref().map(|hold| hold.attempt_id.clone()),
                "candidate": hold.as_ref().map(|hold| hold.candidate.commit.clone()),
                "commands": runs.iter().map(|run| &run.requirement.command).collect::<Vec<_>>(),
                "exit_codes": runs.iter().map(|run| run.exit_code).collect::<Vec<_>>(),
                "free_mib": free_mib,
                "min_free_mib": min_free_mib,
                "outcome": if refusal.is_none() { "fulfilled" } else { "refused" },
                "reason": refusal.map(FulfilmentRefusal::as_str),
                "requeued": requeued,
                "recorded_at": Utc::now().to_rfc3339(),
            }),
            refusal.map(|refusal| format!("{}: {detail}", refusal.as_str())),
        )?;
        Ok(output)
    }

    /// Admit, gate, fetch, check out and run. `Err` is a refusal before any
    /// command ran; `Ok` holds each command's run, refused or not. A command
    /// outside its allowlist refuses the whole hold before any host gate, so
    /// nothing runs and only the refused commands' logs are attached.
    fn fulfil_hold(
        &self,
        task: &Task,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        min_free_mib: u64,
    ) -> Result<Vec<EvidenceRun>, (FulfilmentRefusal, String)> {
        // Persisted holds can predate review admission's path guards. Refuse
        // unsafe locators before running any candidate-controlled script.
        for requirement in &hold.requirements {
            fulfilment_artifact_paths(&requirement.artifact)
                .map_err(|error| (FulfilmentRefusal::ArtifactNotAllowed, error.to_string()))?;
        }
        if let Some((refusal, reason)) = self.fulfilment_owner_refusal() {
            return Err((refusal, reason));
        }
        let owner_required = self.workflow_required_validation_commands();
        let admitted = hold
            .requirements
            .iter()
            .map(|requirement| admit_requirement(requirement, owner_required))
            .collect::<Vec<_>>();
        if admitted.iter().any(Result::is_err) {
            return Ok(hold
                .requirements
                .iter()
                .zip(admitted)
                .filter_map(|(requirement, admitted)| {
                    let (refusal, detail) = admitted.err()?;
                    Some(EvidenceRun::refused(requirement, refusal, detail))
                })
                .collect());
        }
        if let Some((refusal, reason)) = self.fulfilment_host_refusal() {
            return Err((refusal, reason));
        }
        let free = free_mib(&self.paths().state_dir)
            .map_err(|error| (FulfilmentRefusal::DiskInsufficient, error.to_string()))?;
        if free < min_free_mib {
            return Err((
                FulfilmentRefusal::DiskInsufficient,
                format!("{free} MiB free under the state directory; the run needs {min_free_mib}"),
            ));
        }
        let repo_root = &self.paths().repo_root;
        let commit = &hold.candidate.commit;
        let reachable = orbit_engine::review_gate::fetch_landed_commit(repo_root, commit)
            .and_then(|()| orbit_engine::review_gate::revision(repo_root, commit));
        match reachable {
            Ok(revision) if revision == hold.candidate => {}
            Ok(revision) => {
                return Err((
                    FulfilmentRefusal::CandidateUnreachable,
                    format!(
                        "commit {commit} has tree {}, not the held {}",
                        revision.tree, hold.candidate.tree
                    ),
                ));
            }
            Err(error) => return Err((FulfilmentRefusal::CandidateUnreachable, error.to_string())),
        }
        let admitted = admitted.into_iter().flatten().collect::<Vec<_>>();
        let checkout_id = format!("{run_id}-evidence");
        let checkout = if admitted
            .iter()
            .any(|admitted| matches!(admitted, AdmittedCommand::Codeql(_)))
        {
            Some(
                self.create_evidence_checkout(&checkout_id, commit)
                    .map_err(|error| {
                        (FulfilmentRefusal::CandidateUnreachable, error.to_string())
                    })?,
            )
        } else {
            None
        };
        let target_id = format!("{run_id}-evidence-target");
        let runs = hold
            .requirements
            .iter()
            .zip(admitted)
            .map(|(requirement, admitted)| match (admitted, &checkout) {
                (AdmittedCommand::Codeql(args), Some(checkout)) => {
                    self.run_codeql(checkout, requirement, args)
                }
                (AdmittedCommand::Codeql(_), None) => EvidenceRun::refused(
                    requirement,
                    FulfilmentRefusal::CandidateUnreachable,
                    "no evidence checkout was prepared".to_string(),
                ),
                (AdmittedCommand::HostSandboxTest(command), _) => {
                    self.run_host_test(hold, run_id, &target_id, requirement, &command)
                }
            })
            .collect::<Vec<_>>();
        // The checkout and test target are this run's own scratch, database
        // and build included; remove them whatever happened so a held host
        // never accumulates them.
        for scratch in [&checkout_id, &target_id] {
            if let Err(error) = self.remove_evidence_checkout(scratch) {
                tracing::warn!(
                    target: "orbit.core.review",
                    task_id = %task.id,
                    "failed to remove the evidence scratch {scratch}: {error}"
                );
            }
        }
        Ok(runs)
    }

    /// Remove a fulfilment run's checkout or test target, if it exists. It is
    /// the run's own directory, registered nowhere else.
    pub(super) fn remove_evidence_checkout(&self, checkout_id: &str) -> Result<(), OrbitError> {
        let path = recovery_checkout_path(&self.paths().state_dir, checkout_id)?;
        match std::fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(OrbitError::Execution(format!(
                "remove evidence checkout {}: {error}",
                path.display()
            ))),
        }
    }

    /// Run one admitted host sandbox test at the held commit, outside any
    /// sandbox, and judge it with the contract a claimed leaf uses.
    fn run_host_test(
        &self,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        target_id: &str,
        requirement: &ReviewEvidenceRequirement,
        command: &HostSandboxCommand,
    ) -> EvidenceRun {
        let target = match recovery_checkout_path(&self.paths().state_dir, target_id) {
            Ok(target) => target,
            Err(error) => {
                return EvidenceRun::refused(
                    requirement,
                    FulfilmentRefusal::CommandFailed,
                    error.to_string(),
                );
            }
        };
        let ran = run_host_sandbox_test(
            self,
            &self.paths().repo_root,
            &hold.candidate,
            command,
            run_id,
            &target,
        );
        let ran = match ran {
            Ok(ran) => ran,
            Err(error) => {
                return EvidenceRun::refused(
                    requirement,
                    FulfilmentRefusal::CommandFailed,
                    error.to_string(),
                );
            }
        };
        let mut run = EvidenceRun::new(requirement);
        run.exit_code = ran.exit_code;
        run.timed_out = ran.timed_out;
        run.log.extend([
            ("kind".to_string(), json!(requirement.kind)),
            ("os".to_string(), json!(requirement.os)),
            ("host_command".to_string(), json!(ran.host_command)),
            (
                "tests_passed".to_string(),
                json!(ran.judgement.as_ref().ok()),
            ),
            ("output".to_string(), json!(ran.output)),
            ("validation_env".to_string(), ran.environment),
        ]);
        if let Err(refusal) = ran.judgement {
            run.refusal = Some(FulfilmentRefusal::of_host(refusal.reason));
            run.detail = refusal.detail;
        }
        run
    }

    /// Attach each run's log, and every result when all of them passed, in
    /// one write: receipt then requeues the task for a fresh review. A refused
    /// fulfilment attaches only logs, so nothing is accepted.
    fn attach_fulfilment(
        &self,
        task: &Task,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        runs: &[EvidenceRun],
        refusal: Option<FulfilmentRefusal>,
        detail: &str,
    ) -> Result<(), OrbitError> {
        let mut artifacts = Vec::new();
        for run in runs {
            // Check both final store keys at the system-authority write boundary.
            // Build the whole batch first, so a refusal cannot partially write it.
            let (artifact, log_artifact) = fulfilment_artifact_paths(&run.requirement.artifact)?;
            let mut log = json!({
                "schema_version": 1,
                "run_id": run_id,
                "task_id": task.id,
                "attempt_id": hold.attempt_id,
                "tested_head": hold.candidate.commit,
                "tested_tree": hold.candidate.tree,
                "command": run.requirement.command,
                "exit_code": run.exit_code,
                "timed_out": run.timed_out,
                "outcome": if run.refusal.is_none() { "passed" } else { "failed" },
                "reason": run.refusal.map(FulfilmentRefusal::as_str),
                "detail": run.detail,
                "host_os": std::env::consts::OS,
                "recorded_at": Utc::now().to_rfc3339(),
            });
            if let Some(log) = log.as_object_mut() {
                log.extend(run.log.clone());
            }
            artifacts.push(json_artifact(&log_artifact, &log)?);
            if refusal.is_none() {
                let evidence = ReviewExternalEvidence {
                    schema_version: 1,
                    attempt_id: hold.attempt_id.clone(),
                    candidate: hold.candidate.clone(),
                    kind: run.requirement.kind,
                    name: run.requirement.name.clone(),
                    command: run.requirement.command.clone(),
                    outcome: ValidationOutcome::Passed,
                    log_artifact,
                    os: EvidenceHostOs::current(),
                };
                artifacts.push(json_artifact(&artifact, &evidence)?);
            }
        }
        let comment = match refusal {
            None => format!(
                "Owner evidence fulfilment run={run_id} ran every named check at held candidate \
                 `{}` on this Linux host and attached the results and logs. A fresh review \
                 verifies them; nothing was approved.",
                hold.candidate.commit
            ),
            Some(refusal) => format!(
                "Owner evidence fulfilment run={run_id} did not attach a result for held \
                 candidate `{}`: `{}`{}. The evidence hold stays in place{}.",
                hold.candidate.commit,
                refusal.as_str(),
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                },
                if refusal.retryable() {
                    "; a later tick retries it"
                } else {
                    ""
                },
            ),
        };
        self.update_task_as_system(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: artifacts,
                comment: Some(comment),
                ..TaskUpdateParams::default()
            },
            Some(run_id.to_string()),
        )?;
        Ok(())
    }
}

/// `fulfil_review_evidence`: the only step of
/// [`REVIEW_EVIDENCE_FULFILMENT_JOB`]. A refusal is a successful step whose
/// output names the reason; only an Orbit failure fails the step.
pub(crate) fn fulfil_review_evidence(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    let run_id =
        run_id.ok_or_else(|| failed("a fulfilment step must run inside a pipeline run".into()))?;
    runtime
        .fulfil_review_evidence(input, run_id)
        .map_err(|error| failed(error.to_string()))
}

/// Admit `requirement`'s command or refuse it with a typed reason. Nothing
/// outside its kind's closed allowlist ever runs.
fn admit_requirement(
    requirement: &ReviewEvidenceRequirement,
    owner_required: &[String],
) -> Result<AdmittedCommand, (FulfilmentRefusal, String)> {
    match requirement.kind {
        ReviewEvidenceKind::CodeQl => admitted_codeql_args(&requirement.command)
            .map(AdmittedCommand::Codeql)
            .map_err(|detail| (FulfilmentRefusal::CommandNotAllowed, detail)),
        ReviewEvidenceKind::HostSandboxTest if requirement.os == Some(EvidenceHostOs::Linux) => {
            HostSandboxCommand::admit(&requirement.command, owner_required)
                .map(AdmittedCommand::HostSandboxTest)
                .map_err(|refusal| (FulfilmentRefusal::of_host(refusal.reason), refusal.detail))
        }
        _ => Err((
            FulfilmentRefusal::HoldNotCurrent,
            format!("a Linux owner does not fulfil `{}`", requirement.name),
        )),
    }
}

/// `evidence/x.json` logs to `evidence/x.log.json`.
fn log_artifact_path(artifact: &str) -> String {
    match artifact.strip_suffix(".json") {
        Some(stem) => format!("{stem}.log.json"),
        None => format!("{artifact}.log.json"),
    }
}

pub(in crate::application::review) fn fulfilment_artifact_paths(
    artifact: &str,
) -> Result<(String, String), OrbitError> {
    let refused = || {
        OrbitError::InvalidInput(
            "external evidence and log artifacts must have valid, non-reserved review paths"
                .to_string(),
        )
    };
    let artifact = evidence_artifact_path(artifact).ok_or_else(refused)?;
    let log = evidence_artifact_path(&log_artifact_path(&artifact)).ok_or_else(refused)?;
    Ok((artifact, log))
}

fn json_artifact(path: &str, value: &impl serde::Serialize) -> Result<TaskArtifact, OrbitError> {
    Ok(TaskArtifact {
        path: path.to_string(),
        content: serde_json::to_vec_pretty(value)
            .map_err(|error| OrbitError::Execution(format!("serialize {path}: {error}")))?,
        media_type: "application/json".to_string(),
        created_by: None,
    })
}
