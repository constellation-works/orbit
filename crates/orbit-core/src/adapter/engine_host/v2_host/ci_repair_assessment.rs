//! Completed repair assessment at CI intake. Task artifacts propose the binding;
//! referenced diagnostics, command results, delivery state and Git must agree.
//! No command from an artifact or a CI log is executed here.

use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::workflow::JobRunState;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{CI_FAILURE_KEY_TAG_PREFIX, FailureCluster, selected_diagnostic, value_string};
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::task::{TaskListFilter, TaskUpdateParams};

const MAX_OWNERS: usize = 8;
const MAX_ASSESSMENTS: usize = 32;
const MAX_ARTIFACT_BYTES: usize = 1_048_576;
const ASSESSMENT_PATH: &str = "ci-repair-assessment.json";

type AssessmentResult<T> = Result<T, String>;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRef {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RepairAssessment {
    schema_version: u32,
    task_id: String,
    failure_key: String,
    delivery_run_id: String,
    delivery_step_index: u32,
    landed_revision: String,
    // An assessment is specific to immutable observations, not a permanent
    // waiver for a key, test, path or branch.
    observations: Vec<Observation>,
    before: ArtifactRef,
    after: ArtifactRef,
    command: Vec<String>,
    diagnostic_details: Vec<String>,
    coverage_reason: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Observation {
    run_id: String,
    job_id: String,
    checkout: String,
    diagnostic_sha256: String,
    branch: String,
    ref_kind: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ValidationRecord {
    schema_version: u32,
    task_id: String,
    revision: String,
    command: Vec<String>,
    exit_code: i32,
    outcome: String,
    // "retrospective" records newly executed checks, never historical claims.
    origin: String,
    recorded_at: String,
    output: String,
}

pub(super) struct Assessor<'a> {
    runtime: &'a OrbitRuntime,
    source: Source<'a>,
    remaining: usize,
}

pub(super) struct Assessment {
    pub owner: Option<String>,
    pub evidence: Value,
}

impl<'a> Assessor<'a> {
    pub(super) fn new(runtime: &'a OrbitRuntime) -> Self {
        Self {
            runtime,
            source: Source::new(&runtime.paths().repo_root),
            remaining: MAX_ASSESSMENTS,
        }
    }

    pub(super) fn assess(&mut self, cluster: &FailureCluster) -> Assessment {
        let result = self.assess_inner(cluster);
        match result {
            Ok(Some((owner, evidence))) => Assessment {
                owner: Some(owner),
                evidence,
            },
            Ok(None) => Assessment {
                owner: None,
                evidence: Value::Null,
            },
            Err(reason) => Assessment {
                owner: None,
                evidence: json!({"outcome": "unresolved", "failure_key": cluster.failure_key,
                    "cluster_key": cluster.cluster_key, "reason": reason}),
            },
        }
    }

    fn assess_inner(
        &mut self,
        cluster: &FailureCluster,
    ) -> AssessmentResult<Option<(String, Value)>> {
        let tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key);
        let candidates = self
            .runtime
            .task_candidates(
                &TaskListFilter {
                    statuses: Some(vec![TaskStatus::Done]),
                    tags: vec![tag],
                    ..Default::default()
                },
                MAX_OWNERS,
            )
            .map_err(|_| "completed_owner_lookup_unavailable")?;
        if candidates.total > MAX_OWNERS {
            return Err("completed_owner_lookup_budget_exhausted".into());
        }
        let mut unresolved = Vec::new();
        for candidate in candidates.items {
            if self.remaining == 0 {
                return Err("assessment_budget_exhausted".into());
            }
            self.remaining -= 1;
            let owner = self
                .runtime
                .get_task(&candidate.id)
                .map_err(|_| "completed_owner_unavailable")?;
            match self.verify(&owner, cluster) {
                Ok(evidence) => return Ok(Some((owner.id, evidence))),
                Err(reason) => unresolved.push(format!("{}: {reason}", owner.id)),
            }
        }
        if unresolved.is_empty() {
            Ok(None)
        } else {
            Err(unresolved.join("; "))
        }
    }

    fn verify(&self, owner: &Task, cluster: &FailureCluster) -> AssessmentResult<Value> {
        let bytes = self.artifact(owner, ASSESSMENT_PATH)?;
        let assessment: RepairAssessment = decode(&bytes)?;
        let observations = observations(cluster)?;
        validate_binding(owner, cluster, &observations, &assessment)?;

        let before: ValidationRecord = self.reference(owner, &assessment.before)?;
        let after: ValidationRecord = self.reference(owner, &assessment.after)?;
        validate_results(&assessment, &observations, &before, &after)?;
        self.verify_delivery(owner, &assessment)?;
        self.verify_revisions(cluster, &assessment, &before, &after)?;

        let mut sources = cluster.filing_entry(&owner.id)["sources"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        sources.sort_by_key(|source| {
            (
                value_string(source, "run_id"),
                value_string(source, "job_id"),
            )
        });
        sources.dedup();
        Ok(
            json!({"schema_version": 1, "outcome": "covered_by_repair", "task_id": owner.id,
            "failure_key": cluster.failure_key, "cluster_key": cluster.cluster_key,
            "assessment": {"path": ASSESSMENT_PATH, "sha256": sha256(&bytes)},
            "delivery_run_id": assessment.delivery_run_id, "delivery_step_index": assessment.delivery_step_index, "landed_revision": assessment.landed_revision,
            "validated_revision": after.revision, "before": assessment.before, "after": assessment.after,
            "validation_origin": after.origin, "coverage_reason": assessment.coverage_reason,
            "observations": observations, "sources": sources}),
        )
    }

    fn artifact(&self, owner: &Task, path: &str) -> AssessmentResult<Vec<u8>> {
        if path.is_empty()
            || path.len() > 200
            || path.starts_with('/')
            || path.split('/').any(|part| matches!(part, ".." | "." | ""))
        {
            return Err("invalid_artifact_reference".into());
        }
        let manifest = self
            .runtime
            .get_task_artifact_manifest(&owner.id)
            .map_err(|_| "artifact_manifest_unavailable")?;
        let entry = manifest
            .iter()
            .find(|entry| entry.path == path)
            .ok_or("artifact_missing")?;
        if entry.size_bytes > MAX_ARTIFACT_BYTES as u64 {
            return Err("artifact_budget_exhausted".into());
        }
        let artifact = self
            .runtime
            .get_task_artifact(&owner.id, path)
            .map_err(|_| "artifact_unavailable")?
            .ok_or("artifact_missing")?;
        if artifact.content.len() > MAX_ARTIFACT_BYTES {
            return Err("artifact_budget_exhausted".into());
        }
        Ok(artifact.content)
    }

    fn reference<T: DeserializeOwned>(
        &self,
        owner: &Task,
        reference: &ArtifactRef,
    ) -> AssessmentResult<T> {
        let bytes = self.artifact(owner, &reference.path)?;
        if sha256(&bytes) != reference.sha256 {
            return Err("artifact_digest_mismatch".into());
        }
        decode(&bytes)
    }

    fn verify_delivery(&self, owner: &Task, assessment: &RepairAssessment) -> AssessmentResult<()> {
        let run = self
            .runtime
            .show_job_run(&assessment.delivery_run_id)
            .map_err(|_| "delivery_run_unavailable")?;
        let state = self
            .runtime
            .read_run_state(&assessment.delivery_run_id)
            .map_err(|_| "delivery_state_unavailable")?
            .ok_or("delivery_state_missing")?;
        let completion = state
            .step_outputs
            .get(&assessment.delivery_step_index)
            .ok_or("delivery_step_missing")?;
        let assigned = state.initial_input["task_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == &owner.id));
        if run.state != JobRunState::Success
            || state.run_id != run.run_id
            || state.job_id != run.job_id
            || run.job_id != "task_pr_pipeline"
            || !assigned
            || state.step_states.get(&assessment.delivery_step_index) != Some(&JobRunState::Success)
            || owner.job_run_id.as_deref() != Some(assessment.delivery_run_id.as_str())
            || completion["phase"] != "complete"
            || completion["merge"]["merged"] != true
            || completion["merge"]["landed_commit"] != assessment.landed_revision
            || !completion["completed_task_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == &owner.id))
        {
            return Err("delivery_binding_mismatch".into());
        }
        Ok(())
    }

    fn verify_revisions(
        &self,
        cluster: &FailureCluster,
        assessment: &RepairAssessment,
        before: &ValidationRecord,
        after: &ValidationRecord,
    ) -> AssessmentResult<()> {
        let landed = &assessment.landed_revision;
        if after.revision != *landed
            || before.revision != cluster.tested_commit
            || before.revision == *landed
        {
            return Err("validation_revision_mismatch".into());
        }
        for revision in [&before.revision, &after.revision] {
            if !full_revision(revision) {
                return Err("invalid_revision".into());
            }
            let resolved = self
                .source
                .revision(revision)
                .map_err(|_| "revision_unavailable")?;
            if resolved.commit != *revision {
                return Err("revision_mismatch".into());
            }
        }
        let base = self
            .source
            .git(&["merge-base", &before.revision, landed])
            .map_err(|_| "ancestry_unavailable")?;
        if base != before.revision {
            return Err("checkout_not_before_repair".into());
        }
        for run in &cluster.runs {
            let branch = value_string(run, "head_branch");
            let head = value_string(run, "current_ref_head_sha");
            if !full_revision(&head) {
                return Err("branch_head_unavailable".into());
            }
            // Ref-to-head identity comes from the collector, not the artifact.
            // Comparing the current local ref also prevents applying a stale
            // integration observation to a branch which no longer has the repair.
            self.source
                .git(&["check-ref-format", "--branch", &branch])
                .map_err(|_| "invalid_branch")?;
            let current = self
                .source
                .revision(&format!("refs/remotes/origin/{branch}"))
                .or_else(|_| self.source.revision(&format!("refs/heads/{branch}")))
                .map_err(|_| "branch_unavailable")?;
            for branch_head in [&head, &current.commit] {
                let base = self
                    .source
                    .git(&["merge-base", landed, branch_head])
                    .map_err(|_| "branch_ancestry_unavailable")?;
                if base != *landed {
                    return Err("branch_lacks_repair".into());
                }
            }
        }
        Ok(())
    }
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> AssessmentResult<T> {
    serde_json::from_slice(bytes).map_err(|_| "malformed_evidence".into())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn full_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn diagnostic(run: &Value) -> Option<&str> {
    selected_diagnostic(run).or_else(|| {
        (run["log_truncated"] != true)
            .then(|| run["log_excerpt"].as_str())
            .flatten()
    })
}

fn observations(cluster: &FailureCluster) -> AssessmentResult<Vec<Observation>> {
    if cluster.runs.len() > MAX_ASSESSMENTS {
        return Err("observation_budget_exhausted".into());
    }
    let mut observations = Vec::new();
    for run in &cluster.runs {
        let diagnostic = diagnostic(run).ok_or("diagnostic_unavailable")?;
        observations.push(Observation {
            run_id: value_string(run, "run_id"),
            job_id: value_string(run, "job_id"),
            checkout: cluster.tested_commit.clone(),
            diagnostic_sha256: sha256(diagnostic.as_bytes()),
            branch: value_string(run, "head_branch"),
            ref_kind: value_string(run, "ref_kind"),
        });
    }
    observations.sort_by(|a, b| (&a.run_id, &a.job_id).cmp(&(&b.run_id, &b.job_id)));
    observations.dedup();
    Ok(observations)
}

fn validate_binding(
    owner: &Task,
    cluster: &FailureCluster,
    observations: &[Observation],
    assessment: &RepairAssessment,
) -> AssessmentResult<()> {
    if assessment.schema_version != 1
        || owner.status != TaskStatus::Done
        || assessment.task_id != owner.id
        || assessment.failure_key != cluster.failure_key
        || assessment.observations.is_empty()
        || assessment.observations.len() > MAX_ASSESSMENTS
        || assessment.coverage_reason.trim().is_empty()
        || assessment.coverage_reason.len() > 2000
        || assessment.delivery_run_id.is_empty()
        || !full_revision(&assessment.landed_revision)
    {
        return Err("assessment_binding_mismatch".into());
    }
    let mut identities = std::collections::BTreeSet::new();
    if assessment.observations.iter().any(|source| {
        !full_revision(&source.checkout)
            || source.diagnostic_sha256.len() != 64
            || !source
                .diagnostic_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !identities.insert((&source.run_id, &source.job_id))
    }) {
        return Err("contradictory_observation_binding".into());
    }
    if observations.is_empty()
        || observations.iter().any(|source| {
            source.run_id.is_empty()
                || source.job_id.is_empty()
                || source.branch.is_empty()
                || !matches!(source.ref_kind.as_str(), "integration" | "release")
                || !assessment.observations.contains(source)
        })
    {
        return Err("diagnostic_source_mismatch".into());
    }
    if assessment.command.is_empty()
        || assessment.command.len() > 32
        || assessment
            .command
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > 1000)
        || assessment.diagnostic_details.is_empty()
        || assessment.diagnostic_details.len() > 16
    {
        return Err("validation_command_or_diagnostic_missing".into());
    }
    // Require concrete assertion/error details in every supplying diagnostic.
    // The immutable digest above also binds any other assertions in the unit;
    // choosing one convenient test name cannot cover a different source unit.
    if !assessment
        .diagnostic_details
        .iter()
        .any(|detail| detail.contains("assertion") || detail.contains("error"))
    {
        return Err("concrete_diagnostic_missing".into());
    }
    for detail in &assessment.diagnostic_details {
        if detail.len() < 20
            || detail.len() > 4000
            || cluster
                .runs
                .iter()
                .any(|run| !diagnostic(run).is_some_and(|diagnostic| diagnostic.contains(detail)))
        {
            return Err("diagnostic_details_mismatch".into());
        }
    }
    Ok(())
}

fn validate_results(
    assessment: &RepairAssessment,
    observations: &[Observation],
    before: &ValidationRecord,
    after: &ValidationRecord,
) -> AssessmentResult<()> {
    for record in [before, after] {
        if record.schema_version != 1
            || record.task_id != assessment.task_id
            || record.command != assessment.command
            || !full_revision(&record.revision)
            || !matches!(record.origin.as_str(), "recovered" | "retrospective")
            || chrono::DateTime::parse_from_rfc3339(&record.recorded_at).is_err()
            || record.output.is_empty()
            || record.output.len() > MAX_ARTIFACT_BYTES
        {
            return Err("validation_record_mismatch".into());
        }
    }
    if before.exit_code == 0
        || before.outcome != "failed"
        || after.exit_code != 0
        || after.outcome != "passed"
        || after.revision != assessment.landed_revision
        || observations
            .iter()
            .any(|source| source.checkout != before.revision)
        || assessment
            .diagnostic_details
            .iter()
            .any(|detail| !before.output.contains(detail) || after.output.contains(detail))
    {
        return Err("validation_result_mismatch".into());
    }
    Ok(())
}

/// A deterministic per-observation receipt makes retries and reversed snapshots
/// converge without appending comments or rewriting the owner's task meaning.
pub(super) fn retain(
    runtime: &OrbitRuntime,
    owner: &str,
    evidence: &Value,
) -> Result<(), orbit_common::OrbitError> {
    let content = serde_json::to_string(evidence)
        .map_err(|error| orbit_common::OrbitError::InvalidInput(error.to_string()))?;
    let path = format!("ci-repair-observations/{}.json", sha256(content.as_bytes()));
    if runtime.get_task_artifact(owner, &path)?.is_none() {
        runtime.update_task(
            owner,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(path, content)],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/ci_repair_assessment.rs"]
mod tests;
