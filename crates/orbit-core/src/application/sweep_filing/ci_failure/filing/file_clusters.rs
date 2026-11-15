//! Filing: fold dependency-only aggregates into the sibling they name, then
//! walk every cluster once, reusing a covering owner or filing a new
//! `proposed` task through `add_task`.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{
    CiFailureIdentity, TaskComplexity, TaskDecision, TaskPriority, TaskStatus, TaskType,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::contracts::CI_FAILURE_TAG;
use crate::application::sweep_filing::duplicate_tasks::{DuplicateMatchKind, DuplicateTaskMatch};
use crate::application::sweep_filing::{bounded_error, digest, display};
use crate::application::task::{CiSweepFilingEvidence, TaskAddParams};

use super::super::cluster::FailureCluster;
use super::super::dependency_fold;
use super::super::dependency_fold::{
    DependencyRef, cluster_dependency_refs, folded_cluster_indices, sibling_cluster_index,
};
use super::super::evidence::{retryable_pipeline_error, single_retryable_error};
use super::super::fields::value_string;
use super::super::repair_assessment;
use super::super::runner_os;
use super::CI_FAILURE_KEY_TAG_PREFIX;
use super::assess::ClusterOwners;
use super::branch::retain_compiler_observations;
use super::snapshot::FilingSnapshot;

/// Which clusters fold into a sibling, and the owner each folded aggregate
/// is described on.
pub(super) struct DependencyFold {
    pub(super) refs_by_cluster: Vec<Option<Vec<DependencyRef>>>,
    pub(super) folded_indices: BTreeSet<usize>,
    pub(super) sibling_index: BTreeMap<(String, String), usize>,
    pub(super) folded_targets: BTreeMap<usize, Vec<usize>>,
}

pub(super) fn dependency_fold(
    clusters: &[FailureCluster],
    owners: &ClusterOwners,
    max_tasks: usize,
) -> DependencyFold {
    let ClusterOwners {
        operator_assessments,
        duplicate_matches,
        descendant_landings,
        ..
    } = owners;
    // Dependency-only aggregates fold into the sibling they name, but only
    // when that sibling is covered or will be filed within the cap.
    let refs_by_cluster: Vec<_> = clusters.iter().map(cluster_dependency_refs).collect();
    let covered_flags: Vec<bool> = duplicate_matches
        .iter()
        .map(|matched| matched.is_some())
        .collect();
    let blocked_flags: Vec<bool> = operator_assessments
        .iter()
        .zip(descendant_landings)
        .map(|(operator, landing)| operator.withheld.is_some() || landing.is_some())
        .collect();
    let folded_indices = folded_cluster_indices(
        clusters,
        &refs_by_cluster,
        &covered_flags,
        &blocked_flags,
        max_tasks,
    );
    let sibling_index = sibling_cluster_index(clusters);
    // Each folded aggregate is described on the task that covers it, which for
    // a chain A -> B -> C is C's task, not the directly named B.
    let mut folded_targets: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &folded in &folded_indices {
        for owner in dependency_fold::fold_owner_indices(
            &sibling_index,
            &refs_by_cluster,
            &folded_indices,
            folded,
        ) {
            let members = folded_targets.entry(owner).or_default();
            if !members.contains(&folded) {
                members.push(folded);
            }
        }
    }
    DependencyFold {
        refs_by_cluster,
        folded_indices,
        sibling_index,
        folded_targets,
    }
}

/// What the per-cluster loop produced, for the final report.
pub(super) struct FiledClusters {
    pub(super) filed: Vec<CiSweepFilingEvidence>,
    pub(super) pilot_candidates: Vec<CiSweepFilingEvidence>,
    pub(super) skipped_existing: Vec<Value>,
    pub(super) withheld: Vec<Value>,
    pub(super) skipped_over_cap: Vec<Value>,
    pub(super) folded_clusters: Vec<usize>,
}

/// Covered descendant landings join `snapshot.pending_supersession`, and
/// retained repair and compiler observations join `deferred_attribution`.
pub(super) fn file_clusters<F>(
    runtime: &OrbitRuntime,
    clusters: &[FailureCluster],
    owners: ClusterOwners,
    fold: &DependencyFold,
    snapshot: &mut FilingSnapshot<'_>,
    deferred_attribution: &mut Vec<Value>,
    add_task: &mut F,
) -> Result<FiledClusters, OrbitError>
where
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    let evidence = snapshot.evidence;
    let max_tasks = snapshot.max_tasks;
    let audit = &snapshot.audit;
    let pending_supersession = &mut snapshot.pending_supersession;
    let ClusterOwners {
        operator_assessments,
        duplicate_matches,
        duplicate_tasks,
        descendant_landings,
    } = owners;
    let DependencyFold {
        folded_indices,
        folded_targets,
        ..
    } = fold;
    let mut filed = Vec::new();
    let mut pilot_candidates = Vec::new();
    let mut pilot_candidate_ids = BTreeSet::new();
    let mut skipped_existing = Vec::new();
    let mut withheld = Vec::new();
    let mut skipped_over_cap = Vec::new();
    // Two clusters in one snapshot can share a failure key when the same root
    // cause was tested at two commits. The first filing closes the second.
    let mut filed_keys: BTreeSet<String> = BTreeSet::new();
    let mut filed_compiler_sets: BTreeMap<String, String> = BTreeMap::new();
    let mut folded_clusters: Vec<usize> = Vec::new();

    for (
        cluster_index,
        ((((cluster, duplicate_match), duplicate_task), descendant_landing), operator),
    ) in clusters
        .iter()
        .zip(duplicate_matches)
        .zip(duplicate_tasks)
        .zip(descendant_landings)
        .zip(operator_assessments)
        .enumerate()
    {
        if folded_indices.contains(&cluster_index) {
            folded_clusters.push(cluster_index);
            continue;
        }
        if let Some(entry) = operator.withheld {
            withheld.push(entry);
            continue;
        }
        let duplicate_match = duplicate_match.or_else(|| {
            let identity = cluster.open_compiler_identity()?;
            let task_id = filed_compiler_sets.get(&identity)?.clone();
            Some(DuplicateTaskMatch {
                task_id,
                match_kind: DuplicateMatchKind::OpenCompilerDiagnostics,
                evidence: json!({
                    "fingerprint": "ci_open_compiler_diagnostics",
                    "matched_fields": [{"field": "compiler_diagnostic_set", "value": identity}],
                }),
            })
        });
        if let Some(DuplicateTaskMatch {
            task_id,
            match_kind,
            evidence,
        }) = duplicate_match
        {
            if match_kind == DuplicateMatchKind::CoveredByRepair {
                deferred_attribution.extend(repair_assessment::retain(
                    runtime, &task_id, cluster, &evidence,
                )?);
            }
            if match_kind == DuplicateMatchKind::OpenCompilerDiagnostics {
                deferred_attribution
                    .extend(retain_compiler_observations(runtime, &task_id, cluster)?);
            }
            if let Some(existing) = duplicate_task {
                let expected_key_tag =
                    format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key);
                if existing.status == TaskStatus::Proposed
                    && existing.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                    && existing.tags.iter().any(|tag| tag == &expected_key_tag)
                    && pilot_candidate_ids.insert(task_id.clone())
                {
                    pilot_candidates.push(cluster.filing_entry(&task_id));
                }
            }
            skipped_existing.push(json!({
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "task_id": task_id,
                "workflow": cluster.workflow,
                "match_kind": match_kind,
                "match_evidence": evidence,
                "sources": cluster.filing_entry(&task_id).sources,
            }));
            continue;
        }
        if let Some(pending) = descendant_landing {
            pending_supersession.push(pending);
            continue;
        }
        if filed_keys.contains(&cluster.failure_key) {
            skipped_existing.push(json!({
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "task_id": filed
                    .iter()
                    .find(|entry: &&CiSweepFilingEvidence| entry.failure_key == cluster.failure_key)
                    .map(|entry| entry.task_id.as_str())
                    .unwrap_or_default(),
                "workflow": cluster.workflow,
                "match_kind": DuplicateMatchKind::ExactKey,
                "match_evidence": {
                    "fingerprint": "same_sweep_key",
                    "matched_fields": [{
                        "field": "failure_key",
                        "value": cluster.failure_key,
                    }],
                },
            }));
            continue;
        }
        if filed.len() >= max_tasks {
            skipped_over_cap.push(json!({
                "failure_key": cluster.failure_key,
                "workflow": cluster.workflow,
                "job": cluster.job,
                "run_urls": cluster.run_urls(),
            }));
            continue;
        }

        // The failing job's runner routes the repair to a host that can
        // reproduce it; Windows and unknown runners leave it untagged.
        let runners = cluster.runner_os(&runtime.paths().repo_root);
        let os_tags = runner_os::os_tags(&runners);
        let mut tags = vec![
            CI_FAILURE_TAG.to_string(),
            format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key),
            "github-actions".to_string(),
        ];
        tags.extend(os_tags.iter().cloned());
        let mut description = cluster.description(evidence, &runners);
        if let Some(folded) = folded_targets.get(&cluster_index) {
            description.push_str("\n## Folded dependency-only failures\n\n");
            description.push_str(
                "These jobs failed only because this job failed, directly or through other folded \
                 jobs; their only evidence was an `orbit-dependency-evidence` record naming a job \
                 in that chain, so no separate task was filed for them:\n\n",
            );
            for &folded_index in folded {
                let folded_cluster = &clusters[folded_index];
                for run in &folded_cluster.runs {
                    description.push_str(&format!(
                        "- Aggregate job `{}` in run `{}` ({}) failed only on this job.\n",
                        display(&folded_cluster.job),
                        value_string(run, "run_id"),
                        display(&value_string(run, "url")),
                    ));
                }
            }
        }
        if !operator.failed_covers.is_empty() {
            description.push_str("\n## Prior operator covers that did not hold\n\n");
            for cover in &operator.failed_covers {
                description.push_str(&format!(
                    "- Cover `{}` landed at `{}`; the failing checkout contains this fix and the same failure key reproduced.\n",
                    cover["cover"].as_str().unwrap_or_default(),
                    cover["landed_commit"].as_str().unwrap_or_default(),
                ));
            }
        }
        let task_id = add_task(TaskAddParams {
            creation_decision: Some(TaskDecision::CiFailureIdentity(CiFailureIdentity {
                failure_key: cluster.failure_key.clone(),
                compiler_cause_digest: cluster
                    .compiler_cause
                    .as_ref()
                    .map(|cause| digest(&[cause])),
                open_compiler_set: cluster.open_compiler_identity(),
                signature: (!cluster.signature_is_step_fallback)
                    .then(|| display(&cluster.signature).to_string()),
                tested_commit: cluster.tested_commit.clone(),
                sources: cluster.sources.clone(),
            })),
            title: cluster.title(),
            description,
            acceptance_criteria: cluster.acceptance_criteria(),
            tags,
            context_files: cluster.context_files(),
            // Deliberately empty: the evidence is already in the description,
            // so the task ships on the ordinary agent baseline.
            required_tools: Vec::new(),
            crew: None,
            priority: TaskPriority::High,
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Bug),
            // Filing is quarantine, not dispatch authorization. The
            // task-pilot apply boundary is the only CI-sweep path that may
            // promote a relevant, selector-backed repair to backlog.
            status: Some(TaskStatus::Proposed),
            system_created: true,
            ..TaskAddParams::default()
        })
        .map_err(|error| {
            retryable_pipeline_error(
                "task_creation",
                audit,
                vec![single_retryable_error(
                    "task_creation",
                    "orbit.task.add",
                    json!({
                        "failure_key": cluster.failure_key,
                        "run_ids": cluster.run_ids(),
                    }),
                    &bounded_error(&error.to_string()),
                )],
            )
        })?;
        filed_keys.insert(cluster.failure_key.clone());
        if let Some(identity) = cluster.open_compiler_identity() {
            filed_compiler_sets.insert(identity, task_id.clone());
        }
        let mut filing = cluster.filing_entry(&task_id);
        filing.runner_os = Some(json!(runners));
        filing.os_tags = Some(os_tags);
        if !operator.failed_covers.is_empty() {
            filing.failed_covers = Some(json!(operator.failed_covers));
        }
        pilot_candidate_ids.insert(task_id);
        pilot_candidates.push(filing.clone());
        filed.push(filing);
    }

    Ok(FiledClusters {
        filed,
        pilot_candidates,
        skipped_existing,
        withheld,
        skipped_over_cap,
        folded_clusters,
    })
}
