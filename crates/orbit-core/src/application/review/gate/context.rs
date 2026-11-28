//! Inputs both gate steps share: the admitted run, its tasks and their digests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_common::OrbitError;
use orbit_engine::BaseSyncMode;
use orbit_types::task::{Task, requires_security_state_review};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, ReviewAdmission, ReviewBudget, ReviewGateAdmissionOutput,
    ReviewGateNotApplicable, ReviewTiming,
};
use serde_json::{Value, json};

use super::super::{MAX_RESUME_CHAIN, automation_error, lineage_key, resumes_owner};
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::review::admission::{apply_security_state_review, run_review_admission};

/// Shared inputs of both gate steps.
pub(super) struct GateContext {
    pub(super) env: orbit_common::Env,
    pub(super) run_id: String,
    /// First run of the delivery run lineage `run_id` resumes.
    lineage_root: String,
    pub(super) task_ids: Vec<String>,
    pub(super) tasks: Vec<Task>,
    pub(super) workspace_path: PathBuf,
    /// The synchronized base the candidate sits on, pinned at admission.
    base_sha: Option<String>,
    base_branch: String,
    base_sync: BaseSyncMode,
    pub(super) admission: Option<ReviewAdmission>,
    pub(super) workspace_id: String,
    pub(super) repository: String,
    pub(super) task_digests: (BTreeMap<String, String>, String),
    /// Set when this run is a claimed distributed leaf [ORB-13908]: its
    /// task lives in the owner's store, so task writes route to the owner
    /// as claim evidence and selector widening is the owner's at handoff.
    pub(super) claimed: bool,
}

impl GateContext {
    /// Summaries for the reviewer. Until handoff acceptance, the owner's
    /// summary may describe an earlier claimed attempt; the current leaf's
    /// implementer output is checkpointed locally by the compound step.
    pub(super) fn implementer_summaries(
        &self,
        runtime: &OrbitRuntime,
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        let mut summaries = self
            .tasks
            .iter()
            .map(|task| (task.id.to_string(), task.execution_summary.clone()))
            .collect::<BTreeMap<_, _>>();
        if self.claimed {
            let state = runtime.stores().jobs().read_run_state(&self.run_id)?;
            if let Some(implementation) = state
                .as_ref()
                .and_then(|state| state.pipeline.get("implement_one"))
                && let Some(summary) = orbit_engine::review_gate::implementer_summary(
                    &json!({"implementation": implementation}),
                )
            {
                // load verified that this bound run reviews exactly its
                // claimed task, so no other task receives this summary.
                summaries.insert(self.task_ids[0].clone(), summary);
            }
        }
        Ok(summaries)
    }

    pub(super) fn load(
        runtime: &OrbitRuntime,
        input: &Value,
        admission: Option<ReviewAdmission>,
    ) -> Result<Self, OrbitError> {
        let run_id = admitted_run_id(input)?;
        let task_ids = completed_task_ids(input)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if task_ids.is_empty() {
            return Err(OrbitError::InvalidInput(
                "review gate requires input.completed_task_ids".to_string(),
            ));
        }
        let workspace_path = PathBuf::from(required_string(input, "workspace_path")?)
            .canonicalize()
            .map_err(|error| {
                OrbitError::InvalidInput(format!("review gate workspace_path: {error}"))
            })?;
        let base_sha = input
            .get("base_sha")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let base_sync = BaseSyncMode::from_input(input)?;
        let base_branch = input
            .get("base")
            .and_then(Value::as_str)
            .map(|base| base.trim().trim_start_matches("origin/").to_string())
            .filter(|base| !base.is_empty())
            .unwrap_or_else(|| "main".to_string());

        let claim_id = claimed_leaf_claim(runtime, &run_id, &task_ids)?;
        // A generic resume keeps the task bound to the run that created the
        // reused worktree, which delivery steps name as `job_run_id`. Only a
        // same-job retry descendant of that owner reviews on its behalf.
        let checkpoint_owner = input
            .get("job_run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|owner| !owner.is_empty() && *owner != run_id);
        let resumed_owner = match checkpoint_owner {
            Some(owner) if claim_id.is_none() && resumes_owner(runtime, &run_id, owner)? => {
                Some(owner)
            }
            _ => None,
        };
        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in &task_ids {
            // A claimed leaf reads the owner's task through its binding; the
            // claim, checked above, is what ties that task to this run.
            let task = runtime.get_task(task_id)?;
            if claim_id.is_none()
                && (!runtime.task_run_is_local(&task)
                    || (task.job_run_id.as_deref() != Some(run_id.as_str())
                        && (resumed_owner.is_none()
                            || task.job_run_id.as_deref() != resumed_owner)))
            {
                return Err(OrbitError::Execution(format!(
                    "review gate: task '{task_id}' no longer belongs to run '{run_id}'"
                )));
            }
            tasks.push(task);
        }
        let claimed_leaf = if runtime.role().is_worker() {
            Some(runtime.current_claimed_leaf()?)
        } else {
            None
        };
        let mut admission = match admission {
            Some(admission) => Some(admission),
            None => reconcile_claimed_review_admission(
                claimed_leaf.as_ref(),
                &run_id,
                run_review_admission(runtime, &run_id)?,
            )?,
        };
        if let Some(admission) = &mut admission
            && tasks
                .iter()
                .any(|task| requires_security_state_review(&task.tags))
        {
            apply_security_state_review(admission);
        }
        let task_digests = compute_task_digests(&tasks)?;
        // A claim is one delivery attempt, never resumed: it keys its own
        // lineage, held in this executor's review store.
        let lineage_root = match &claim_id {
            Some(claim_id) => claim_id.clone(),
            None => lineage_root(runtime, &run_id)?,
        };
        let repository = Source::new(
            &runtime.paths().repo_root,
            runtime.execution_env_policy().process_env(),
        )
        .repository()
        .map_err(automation_error)?;
        Ok(Self {
            env: runtime.env().clone(),
            run_id,
            lineage_root,
            task_ids,
            tasks,
            workspace_path,
            base_sha,
            base_branch,
            base_sync,
            admission,
            workspace_id: runtime.workspace_id()?,
            repository,
            task_digests,
            claimed: claim_id.is_some(),
        })
    }

    /// The base commit the candidate is pinned against: the explicit pin when
    /// a step supplied one, otherwise the merge base with the synchronized
    /// base ref.
    pub(super) fn base_sha(&self) -> Result<String, OrbitError> {
        match &self.base_sha {
            Some(base_sha) => Ok(base_sha.clone()),
            None => Ok(orbit_engine::review_gate::synchronized_base(
                &self.env,
                &self.workspace_path,
                &self.base_branch,
                self.base_sync,
            )?
            .commit),
        }
    }

    /// The ref the base is synchronized from, as delivery names it: the
    /// remote-tracking branch under remote sync, the local branch otherwise.
    /// A red-base hold watches it to learn when the base turns green.
    pub(super) fn base_ref(&self) -> String {
        match self.base_sync {
            BaseSyncMode::Local => self.base_branch.clone(),
            BaseSyncMode::Remote => format!("origin/{}", self.base_branch),
        }
    }

    pub(super) fn lineage_key(&self) -> String {
        lineage_key(
            &self.workspace_id,
            &self.task_ids,
            &self.base_branch,
            &self.lineage_root,
        )
    }

    pub(super) fn refresh_task_digests(&mut self) -> Result<(), OrbitError> {
        self.task_digests = compute_task_digests(&self.tasks)?;
        Ok(())
    }
}

/// Cross-check a claimed leaf's persisted review admission against the
/// durable pull admission's owner-captured ship contract.
///
/// Non-workers (`leaf: None`) keep `persisted` unchanged. A claimed worker
/// fails closed if its claim requires before-PR or before-landing review
/// without a matching admission or review contract, or if the persisted
/// admission disagrees with the owner's captured timing. Legacy owner-off
/// claims whose leaf predates explicit `timing = none` persistence resolve to
/// a claim-sourced [`ReviewTiming::None`] admission without reading follower
/// configuration.
pub(super) fn reconcile_claimed_review_admission(
    leaf: Option<&crate::application::job::claimed::ClaimedLeaf>,
    run_id: &str,
    persisted: Option<ReviewAdmission>,
) -> Result<Option<ReviewAdmission>, OrbitError> {
    let Some(leaf) = leaf else {
        return Ok(persisted);
    };
    if leaf.binding.bound_run_id != run_id {
        return Err(OrbitError::PolicyDenied(format!(
            "review gate: a claimed leaf reviews only its bound run '{}', not '{run_id}'",
            leaf.binding.bound_run_id
        )));
    }
    let ship = &leaf.admission.request.ship;
    let expected = ship.review_timing();
    match (expected, persisted) {
        (ReviewTiming::None, Some(admission)) if admission.timing == ReviewTiming::None => {
            Ok(Some(admission))
        }
        (ReviewTiming::None, None) => {
            let gates = leaf.owner_gates();
            Ok(Some(ReviewAdmission {
                contract_version: REVIEW_CONTRACT_VERSION,
                policy_version: 0,
                timing: ReviewTiming::None,
                timing_source: "claim".to_string(),
                crew: None,
                crew_source: "claim".to_string(),
                crew_pool: Vec::new(),
                budget: ReviewBudget::default(),
                required_validation_commands: gates
                    .as_ref()
                    .map(|gates| gates.required_validation_commands.clone()),
                baseline_commands: gates
                    .map(|gates| gates.baseline_commands)
                    .unwrap_or_default(),
                host_evidence: Vec::new(),
                selective_before_landing: None,
                captured_at: chrono::Utc::now(),
            }))
        }
        (ReviewTiming::None, Some(admission)) => Err(OrbitError::CapabilityDenied(format!(
            "review_contract_mismatch: the claim captured review timing `none` but this leaf \
             carries review admission `{}`",
            admission.timing.as_str()
        ))),
        (required, _) if ship.review.is_none() => Err(OrbitError::CapabilityDenied(format!(
            "review_contract_mismatch: the claim captured review timing `{}` without a review \
             contract",
            required.as_str()
        ))),
        (required, None) => Err(OrbitError::CapabilityDenied(format!(
            "review_admission_missing: the claim captured review timing `{}` but this leaf has \
             no captured review admission",
            required.as_str()
        ))),
        (required, Some(admission)) if admission.timing != required => {
            Err(OrbitError::CapabilityDenied(format!(
                "review_contract_mismatch: the claim captured review timing `{}` but this leaf \
                 carries review admission `{}`",
                required.as_str(),
                admission.timing.as_str()
            )))
        }
        (_, Some(admission)) => Ok(Some(admission)),
    }
}

/// The claim `run_id` executes, when this process is a claimed leaf. The
/// gate reviews exactly the claimed task as that leaf's bound run, or not at
/// all.
fn claimed_leaf_claim(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_ids: &[String],
) -> Result<Option<String>, OrbitError> {
    if !runtime.role().is_worker() {
        return Ok(None);
    }
    let leaf = runtime.current_claimed_leaf()?;
    if leaf.binding.bound_run_id != run_id || task_ids != [leaf.claim.task_id.clone()] {
        return Err(OrbitError::PolicyDenied(format!(
            "review gate: a claimed leaf reviews only its claimed task as its bound run; run \
             '{run_id}' asked for {task_ids:?}"
        )));
    }
    Ok(Some(leaf.claim.claim_id))
}

/// The first run of the resume chain `run_id` belongs to. A missing source
/// run ends the chain at the last run found.
fn lineage_root(runtime: &OrbitRuntime, run_id: &str) -> Result<String, OrbitError> {
    let mut root = run_id.to_string();
    for _ in 0..MAX_RESUME_CHAIN {
        // A fresh admission carrying a published owner candidate spends the
        // original lineage's review budget; adoption conveys no green verdict.
        let adopted = runtime.read_owner_publication_binding(&root, false)?;
        let source = adopted
            .map(|binding| binding.source_run_id)
            .or(runtime
                .get_job_run_backend(&root)?
                .and_then(|run| run.retry_source_run_id))
            .filter(|source| !source.trim().is_empty() && *source != root);
        match source {
            Some(source) => root = source,
            None => break,
        }
    }
    Ok(root)
}

fn compute_task_digests(tasks: &[Task]) -> Result<(BTreeMap<String, String>, String), OrbitError> {
    let mut digests = BTreeMap::new();
    for task in tasks {
        digests.insert(
            task.id.to_string(),
            task_meaning_digest(task).map_err(automation_error)?,
        );
    }
    let combined = combined_task_meaning_digest(
        &digests
            .iter()
            .map(|(id, digest)| (id.clone(), digest.clone()))
            .collect::<Vec<_>>(),
    )
    .map_err(automation_error)?;
    Ok((digests, combined))
}

fn required_string(input: &Value, key: &str) -> Result<String, OrbitError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| OrbitError::InvalidInput(format!("review gate requires input.{key}")))
}

pub(super) fn completed_task_ids(input: &Value) -> impl Iterator<Item = &str> {
    input
        .get("completed_task_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// The admitted job that captured review policy and owns the candidate.
///
/// Dispatcher injects the executing run as `run_id`. A caller may pass a
/// stable worktree token as `job_run_id`; that token is not a run record.
pub(super) fn admitted_run_id(input: &Value) -> Result<String, OrbitError> {
    let job_run_id = required_string(input, "job_run_id")?;
    let injected = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(injected.unwrap_or(job_run_id))
}

pub(super) fn not_applicable(
    reason: &str,
    admission: Option<&ReviewAdmission>,
) -> ReviewGateAdmissionOutput {
    ReviewGateAdmissionOutput::NotApplicable(ReviewGateNotApplicable::new(reason, admission))
}
