//! Reuse only the successful prefix of this worker's unchanged publication.
use super::*;
use crate::application::job::ReviewAdmissionHook;
use orbit_engine::CompletionObservationAdmission;
use orbit_types::task::ArtifactWriter;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewTiming,
};
use serde::{Deserialize, Serialize};

/// Keep the original strict hash for old receipts. New receipts also bind
/// every non-review input independently, so review-only config drift can be
/// judged against the captured candidate's actual obligations.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompletionPolicy {
    schema_version: u32,
    strict: String,
    non_review: String,
}

impl CompletionPolicy {
    fn matches(&self, captured: &str, observation: bool) -> bool {
        if captured == self.strict {
            return true; // schema 2, before separate review-drift evidence
        }
        let Ok(previous) = serde_json::from_str::<Self>(captured) else {
            return false;
        };
        previous.schema_version == self.schema_version
            && (previous.strict == self.strict
                || (observation && previous.non_review == self.non_review))
    }
}

impl OrbitRuntime {
    pub(crate) fn published_completion_policy(
        &self,
        run_id: &str,
        task_ids: &[String],
    ) -> Result<Option<String>, OrbitError> {
        if self.worker_invocation().is_some() {
            return Ok(None);
        }
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        if run.job_id != "task_pr_pipeline" {
            return Ok(None);
        }
        let tasks = task_ids
            .iter()
            .map(|id| self.get_task(id))
            .collect::<Result<Vec<_>, _>>()?;
        let meanings = tasks
            .iter()
            .map(task_digest)
            .collect::<Result<Vec<_>, _>>()?;
        // Only the digest leaves the host. Environment values may contain secrets.
        let environment = self.validation_subprocess_environment();
        let mut material = json!({
            // Older captures predate write-ahead completion mutation provenance.
            "schema_version": 2, "run_id": run_id, "input": run.input,
            "tasks": task_ids, "meanings": meanings,
            "definition": self.source_repair_definition(&run)?,
            "gates": self.host_validation_gates().gates,
            "review": self.operation_policy(),
            "crews": self.context.settings().crews(),
            "validation_policy": format!("{:?}", self.context.settings().validation_env()),
            "environment": format!("{environment:?}"),
        });
        let strict = digest(&material)?;
        material["review"] = json!({"version": self.operation_policy().version});
        // Baseline replays belong to review, not native validation. Their
        // candidate obligations are compared separately with the admission.
        material["gates"]["baseline_commands"] = json!([]);
        let policy = CompletionPolicy {
            schema_version: 3,
            strict,
            non_review: digest(&material)?,
        };
        serde_json::to_string(&policy)
            .map(Some)
            .map_err(|error| refuse(error.to_string()))
    }

    pub(crate) fn authenticate_published_completion(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<bool, OrbitError> {
        self.classify_published_completion(run_id, task_id, step_id, false)
            .map(|admission| matches!(admission, CompletionObservationAdmission::Retry))
    }

    pub(crate) fn classify_published_completion(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
        observation: bool,
    ) -> Result<CompletionObservationAdmission, OrbitError> {
        let source = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| refuse("completion run missing"))?;
        let task = self.get_task(task_id)?;
        self.owner_source_identity(&source, &task)?;
        // This is an in-worker continuation, never authority to start or rebind a worker.
        if source.state != JobRunState::Running
            || source.pid != Some(std::process::id())
            || source.finished_at.is_some()
            || source.retry_source_run_id.is_some()
            || task.job_run_id.as_deref() != Some(run_id)
            || task.status != TaskStatus::Review
            || source
                .input
                .as_ref()
                .is_none_or(|i| i["completion"] != "done" || i.get(SOURCE_REPAIR_KEY).is_some())
        {
            return Err(refuse("completion no longer belongs to this active owner"));
        }
        let state = self
            .read_run_state(run_id)?
            .ok_or_else(|| refuse("completion state missing"))?;
        if orbit_types::workflow::pr_completion_has_mutation_provenance(
            &self.get_task_history(task_id)?,
        ) {
            return Err(refuse(
                "PR completion repair or mutation provenance requires governed candidate recovery",
            ));
        }
        if state.failure_activity_checkpoint.is_some() {
            return Err(refuse(
                "failure_activity_checkpoint requires governed candidate recovery",
            ));
        }
        let job = self.source_repair_definition(&source)?;
        let publication = job.steps.iter().position(|step| {
            matches!(&step.body, JobV2StepBody::Target(t)
                if matches!(&t.spec, ActivityV2Spec::Deterministic(s) if s.action == "git_push"))
        });
        for (step_id, checkpoint) in &state.rebase_recovery_checkpoints {
            // Routine preparation predates the published candidate, whose head,
            // base and validation receipts are still authenticated below.
            // Unknown steps, rewrite recovery and post-publication checkpoints
            // retain the governed candidate recovery route.
            let preparation = checkpoint["kind"] == "delivery_preparation"
                && publication.is_some_and(|index| {
                    job.steps[..index].iter().any(|step| {
                        step.id == *step_id
                            && matches!(&step.body, JobV2StepBody::Target(t)
                                if matches!(&t.spec, ActivityV2Spec::Deterministic(s)
                                    if s.action == "pr_prepare"))
                    })
                });
            if !preparation {
                return Err(refuse(format!(
                    "rebase recovery checkpoint at `{step_id}` requires governed candidate recovery"
                )));
            }
        }
        let index = job
            .steps
            .iter()
            .position(|s| s.id == step_id)
            .ok_or_else(|| refuse("completion step missing"))?;
        if !matches!(&job.steps[index].body, JobV2StepBody::Target(t)
            if t.activity_name.as_deref() == Some("pr_complete")
            && matches!(&t.spec, ActivityV2Spec::Deterministic(s) if s.action == "pr_complete"))
            || job.steps[..index].iter().enumerate().any(|(i, _)| {
                !matches!(
                    state.step_states.get(&(i as u32)),
                    Some(JobRunState::Success | JobRunState::Skipped)
                )
            })
            || state.final_recovery.as_ref().is_none_or(|r| {
                r.task_id != task_id || r.failed_step_id != step_id || r.decision.is_some()
            })
        {
            return Err(refuse("completion prefix or recovery admission incomplete"));
        }
        let checkpoint = |id: &str, action: &str| {
            observation::authenticated_checkpoint(&job, &state, id, action)
        };
        // Reviewer repairs deliberately retain the existing fresh-candidate route.
        if ["review_gate_settle", "landing_review_gate_settle"]
            .iter()
            .any(|id| {
                state
                    .pipeline
                    .get(*id)
                    .is_some_and(|v| v["reviewer_fixed"] == true)
            })
        {
            return Err(refuse(
                "reviewer repair requires governed candidate recovery",
            ));
        }
        let (_, _, binding) = self.observe_published_source(source.clone(), task.clone(), false)?;
        let worktree = checkpoint("worktree", "worktree_setup")?;
        let checkout = std::path::Path::new(field(worktree, "workspace_path")?);
        let git = |args: &[&str]| -> Result<String, OrbitError> {
            let result = run_git(checkout, args)?;
            if !result.success {
                return Err(refuse("completion Git observation failed"));
            }
            Ok(result.stdout.trim().to_string())
        };
        // [ORB-17140] A base that advanced after publication is not drift.
        // The receipts and review below stay bound to the base they judged,
        // and the resumed `pr_complete` applies its own base-freshness and
        // required-check gates to the published head before any merge.
        if git(&["rev-parse", "HEAD"])? != binding.head_sha
            || git(&["branch", "--show-current"])? != binding.branch
            || !git(&["status", "--porcelain"])?.is_empty()
        {
            return Err(refuse("completion candidate or checkout changed"));
        }
        let validation = checkpoint("validate", "candidate_validate")?;
        let commands = self.required_validation_commands();
        let policy = self
            .published_completion_policy(run_id, &[task_id.into()])?
            .ok_or_else(|| refuse("completion policy unavailable"))?;
        let policy: CompletionPolicy =
            serde_json::from_str(&policy).map_err(|error| refuse(error.to_string()))?;
        // A workspace that requires no command has no receipt to check, but
        // its capture still binds the effective completion policy.
        let validated = if commands.is_empty() {
            validation["decision"] == "skipped_no_required_commands"
                && validation["validation"] == json!([])
        } else {
            validation["decision"] == "passed"
                && validation["tested_head"] == binding.head_sha
                && validation["base_sha"] == binding.base_sha
                && validation["branch"] == binding.branch
        };
        if !validated
            || validation["commands"] != json!(commands)
            || !validation["completion_policy"]
                .as_str()
                .is_some_and(|captured| policy.matches(captured, observation))
        {
            return Err(refuse(
                "native validation candidate or effective policy changed",
            ));
        }
        let references: Vec<orbit_types::workflow::handoff::HandoffArtifactRef> =
            serde_json::from_value(validation["validation"].clone())
                .map_err(|e| refuse(e.to_string()))?;
        let manifest = self.get_task_artifact_manifest(task_id)?;
        if references.len() != commands.len() {
            return Err(refuse("native validation receipts incomplete"));
        }
        for (i, (reference, command)) in references.iter().zip(&commands).enumerate() {
            if reference.path != format!("validation/{run_id}/{i}.json")
                || !manifest.iter().any(|entry| {
                    entry.path == reference.path
                        && entry.sha256 == reference.sha256
                        && entry.writer == Some(ArtifactWriter::System)
                })
            {
                return Err(refuse("native validation receipt lacks host provenance"));
            }
            let artifact = self
                .get_task_artifact(task_id, &reference.path)?
                .ok_or_else(|| refuse("native validation log missing"))?;
            let log: Value =
                serde_json::from_slice(&artifact.content).map_err(|e| refuse(e.to_string()))?;
            if sha256_hex(&artifact.content) != reference.sha256
                || log["schema_version"] != 1
                || !log["output"].is_string()
                || log["run_id"] != run_id
                || log["task_ids"] != json!([task_id])
                || log["tested_head"] != binding.head_sha
                || log["base_sha"] != binding.base_sha
                || log["branch"] != binding.branch
                || log["command"] != *command
                || log["exit_code"] != 0
                || log["timed_out"] != false
                || !log["missing_tool"].is_null()
                || !log["deferred_notice"].is_null()
                || !log["host_output_refusal"].is_null()
            {
                return Err(refuse(
                    "native validation log is not a complete successful host result",
                ));
            }
        }
        let admission = self.captured_review_admission(run_id)?;
        let timing = admission.as_ref().map(|a| a.timing);
        let policy_change = if timing.is_none() {
            (self.operation_policy().review_before_pr.value
                || self.operation_policy().review_before_landing.value)
                .then(|| {
                    "review policy has no captured admission for the newly required gate".into()
                })
        } else if observation {
            self.completion_review_mismatch(&source)
        } else {
            self.upgrade_resume_review_mismatch(&source)
        };
        if !observation && policy_change.is_some() {
            return Err(refuse(if timing.is_none() {
                "review policy has no captured admission"
            } else {
                "captured review policy changed"
            }));
        }
        // Even policy drift cannot admit a stale or missing captured certificate.
        // Check all original review evidence before choosing governed recovery.
        if let Some(timing @ (ReviewTiming::BeforePr | ReviewTiming::BeforeLanding)) = timing {
            let id = if timing == ReviewTiming::BeforePr {
                "review_gate_settle"
            } else {
                "landing_review_gate_settle"
            };
            let settle = checkpoint(id, "review_gate_settle")?;
            if settle["gate"] != "passed"
                || settle["reviewed_head_sha"] != binding.head_sha
                || settle["reviewed_base_sha"] != binding.base_sha
                || settle["reviewer_fixed"] != false
                || !manifest.iter().any(|a| {
                    a.path == REVIEW_GATE_ARTIFACT && a.writer == Some(ArtifactWriter::System)
                })
            {
                return Err(refuse(
                    "completion review disposition is not an unchanged approval",
                ));
            }
            let artifact = self
                .get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)?
                .ok_or_else(|| refuse("review certificate missing"))?;
            let cert: ReviewCertificate =
                serde_json::from_slice(&artifact.content).map_err(|e| refuse(e.to_string()))?;
            if cert.schema_version != REVIEW_CONTRACT_VERSION
                || !manifest.iter().any(|entry| {
                    entry.path == REVIEW_GATE_ARTIFACT
                        && entry.sha256 == sha256_hex(&artifact.content)
                        && entry.writer == Some(ArtifactWriter::System)
                })
                || cert.final_candidate.tree != git(&["rev-parse", "HEAD^{tree}"])?
                || cert.base.tree != git(&["rev-parse", &format!("{}^{{tree}}", binding.base_sha)])?
                || !cert.verdict.passed()
                || !cert.validation_complete
                || cert.final_candidate.commit != binding.head_sha
                || cert.base.commit != binding.base_sha
                || cert.attempt_id != settle["attempt_id"]
                || cert.task_ids != [task_id]
                || cert.task_meaning_digest
                    != orbit_automation::review::combined_task_meaning_digest(&[(
                        task.id.clone(),
                        orbit_automation::review::task_meaning_digest(&task)
                            .map_err(|e| refuse(e.to_string()))?,
                    )])
                    .map_err(|e| refuse(e.to_string()))?
            {
                return Err(refuse("completion review certificate disagrees"));
            }
        }
        Ok(match policy_change {
            Some(reason) => CompletionObservationAdmission::PolicyChanged { reason },
            None => CompletionObservationAdmission::Retry,
        })
    }
}
