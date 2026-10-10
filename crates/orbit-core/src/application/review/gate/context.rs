//! Inputs both gate steps share: the admitted run, its tasks and their digests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::ReviewAdmission;
use serde_json::{Value, json};

use super::super::admission::run_review_admission;
use super::super::{automation_error, lineage_key};
use crate::OrbitRuntime;
use crate::application::automation::source::Source;

/// Shared inputs of both gate steps.
pub(super) struct GateContext {
    pub(super) run_id: String,
    /// First run of the delivery run lineage `run_id` resumes.
    lineage_root: String,
    pub(super) task_ids: Vec<String>,
    pub(super) tasks: Vec<Task>,
    pub(super) workspace_path: PathBuf,
    /// The synchronized base the candidate sits on, pinned at admission.
    base_sha: Option<String>,
    base_branch: String,
    base_sync: String,
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
    pub(super) fn load(
        runtime: &OrbitRuntime,
        input: &Value,
        admission: Option<ReviewAdmission>,
    ) -> Result<Self, OrbitError> {
        let run_id = admitted_run_id(input)?;
        let task_ids = input
            .get("completed_task_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|ids| !ids.is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "review gate requires input.completed_task_ids".to_string(),
                )
            })?;
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
        let base_sync = input
            .get("base_sync")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("remote")
            .to_string();
        let base_branch = input
            .get("base")
            .and_then(Value::as_str)
            .map(|base| base.trim().trim_start_matches("origin/").to_string())
            .filter(|base| !base.is_empty())
            .unwrap_or_else(|| "main".to_string());

        let claim_id = claimed_leaf_claim(runtime, &run_id, &task_ids)?;
        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in &task_ids {
            // A claimed leaf reads the owner's task through its binding; the
            // claim, checked above, is what ties that task to this run.
            let task = runtime.get_task(task_id)?;
            if claim_id.is_none() && task.job_run_id.as_deref() != Some(run_id.as_str()) {
                return Err(OrbitError::Execution(format!(
                    "review gate: task '{task_id}' no longer belongs to run '{run_id}'"
                )));
            }
            tasks.push(task);
        }
        let admission = match admission {
            Some(admission) => Some(admission),
            None => run_review_admission(runtime, &run_id)?,
        };
        let task_digests = compute_task_digests(&tasks)?;
        // A claim is one delivery attempt, never resumed: it keys its own
        // lineage, held in this executor's review store.
        let lineage_root = match &claim_id {
            Some(claim_id) => claim_id.clone(),
            None => lineage_root(runtime, &run_id)?,
        };
        let repository = Source::new(&runtime.paths().repo_root)
            .repository()
            .map_err(automation_error)?;
        Ok(Self {
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
                &self.workspace_path,
                &self.base_branch,
                &self.base_sync,
            )?
            .commit),
        }
    }

    /// The ref the base is synchronized from, as delivery names it: the
    /// remote-tracking branch under remote sync, the local branch otherwise.
    /// A red-base hold watches it to learn when the base turns green.
    pub(super) fn base_ref(&self) -> String {
        if self.base_sync == "local" {
            self.base_branch.clone()
        } else {
            format!("origin/{}", self.base_branch)
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

/// The claim `run_id` executes, when this process is a claimed leaf. The
/// gate reviews exactly the claimed task as that leaf's bound run, or not at
/// all.
fn claimed_leaf_claim(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_ids: &[String],
) -> Result<Option<String>, OrbitError> {
    if runtime.worker_invocation().is_none() {
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

/// Longest resume chain followed back to its first run; a longer chain keys
/// the lineage on the oldest run reached.
const MAX_RESUME_CHAIN: usize = 64;

/// The first run of the resume chain `run_id` belongs to. A missing source
/// run ends the chain at the last run found.
fn lineage_root(runtime: &OrbitRuntime, run_id: &str) -> Result<String, OrbitError> {
    let mut root = run_id.to_string();
    for _ in 0..MAX_RESUME_CHAIN {
        let source = runtime
            .get_job_run_backend(&root)?
            .and_then(|run| run.retry_source_run_id)
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

pub(super) fn not_applicable(reason: &str, admission: Option<&ReviewAdmission>) -> Value {
    json!({
        "applies": false,
        "reason": reason,
        "timing": admission.map(|admission| admission.timing.as_str()),
        "timing_source": admission.map(|admission| admission.timing_source.clone()),
    })
}
