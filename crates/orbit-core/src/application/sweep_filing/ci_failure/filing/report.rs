//! Outcome payloads: the ending when no cluster is complete enough to file,
//! and the `current_failures` report with every folded aggregate and the
//! task that covers it.

use serde_json::{Value, json};

use orbit_common::OrbitError;

use super::super::branch_observation::BranchRetention;
use super::super::cluster::FailureCluster;
use super::super::dependency_fold;
use super::super::dependency_fold::sibling_cluster_for;
use super::super::evidence::{
    deferral_is_bounded, deferred_errors, filing_audit, pending_audit, retryable_pipeline_error,
};
use super::file_clusters::{DependencyFold, FiledClusters};
use super::snapshot::{FilingSnapshot, empty_snapshot_detail};
use super::{OUTCOME_CURRENT_FAILURES, OUTCOME_DEFERRED, OUTCOME_NO_CURRENT_FAILURE};

/// No cluster is complete enough to file.
pub(super) fn empty_clusters_outcome(
    snapshot: FilingSnapshot<'_>,
    retention: BranchRetention,
) -> Result<Value, OrbitError> {
    let FilingSnapshot {
        capability,
        audit,
        deferred,
        evidence_incomplete,
        inconclusive,
        infrastructure,
        already_repaired,
        pending_supersession,
        excluded_branch_failures,
        ..
    } = snapshot;
    // Nothing was complete enough to file. [ORB-15517] When every gap is
    // a run-scoped collection error, the sweep defers them to a later one
    // rather than failing: `deferred` is never a clean
    // `no_current_failure`, and a gap that repeats for three sweeps still
    // degrades or escalates in collection. Any other gap has no such
    // bound, so it stays a retryable error.
    if !deferred.is_empty() && !deferral_is_bounded(&deferred) {
        return Err(retryable_pipeline_error(
            "collection_or_investigation",
            &audit,
            deferred_errors(&deferred),
        ));
    }
    let outcome = if !deferred.is_empty() {
        OUTCOME_DEFERRED
    } else if retention.attributed.is_empty()
        && retention.deferred_attribution.is_empty()
        && excluded_branch_failures.is_empty()
        && evidence_incomplete.is_empty()
        && retention.observation_errors.is_empty()
        && infrastructure.is_empty()
    {
        OUTCOME_NO_CURRENT_FAILURE
    } else {
        OUTCOME_CURRENT_FAILURES
    };
    let detail = if !infrastructure.is_empty() && deferred.is_empty() {
        "the current failures carry a declared infrastructure signature, so no task is \
         filed for them; nothing here is a CI pass"
    } else if deferred.is_empty() {
        empty_snapshot_detail(
            inconclusive.is_empty(),
            retention.deferred_attribution.is_empty(),
        )
    } else {
        "every current failure's evidence is incomplete for a retryable, run-scoped reason; \
         each is deferred to a later sweep, which files it once its evidence is complete, \
         and nothing here is a CI pass"
    };
    Ok(json!({
        "outcome": outcome,
        "capability": capability,
        "clusters": 0,
        "filed_count": 0,
        "filed": [],
        "pilot_candidate_count": 0,
        "pilot_candidates": [],
        "skipped_existing": [],
        "withheld": [],
        "skipped_over_cap": [],
        "deferred_count": deferred.len(),
        "deferred": deferred,
        "evidence_incomplete": evidence_incomplete,
        "inconclusive": inconclusive,
        "infrastructure": infrastructure,
        "already_repaired": already_repaired,
        "pending_supersession": pending_supersession,
        "attributed": retention.attributed,
        "deferred_attribution": retention.deferred_attribution,
        "excluded_branch_failures": excluded_branch_failures,
        "audit": pending_audit(audit, &pending_supersession),
        "detail": detail,
    }))
}

pub(super) fn current_failures_outcome(
    snapshot: FilingSnapshot<'_>,
    clusters: &[FailureCluster],
    fold: &DependencyFold,
    filed: FiledClusters,
    attributed: Vec<Value>,
    deferred_attribution: Vec<Value>,
    repair_assessments: Vec<Value>,
) -> Result<Value, OrbitError> {
    let FilingSnapshot {
        capability,
        max_tasks,
        audit,
        deferred,
        evidence_incomplete,
        inconclusive,
        infrastructure,
        already_repaired,
        pending_supersession,
        excluded_branch_failures,
        ..
    } = snapshot;
    let FiledClusters {
        filed,
        pilot_candidates,
        skipped_existing,
        withheld,
        skipped_over_cap,
        folded_clusters,
    } = filed;
    let DependencyFold {
        refs_by_cluster,
        folded_indices,
        sibling_index,
        ..
    } = fold;
    // Report every folded aggregate with the task that covers it: the owner its
    // dependency chain reaches, which can be several folds away from its record.
    let task_id_for_failure_key = |failure_key: &str| -> Option<String> {
        filed
            .iter()
            .find(|entry| entry.failure_key == failure_key)
            .map(|entry| entry.task_id.clone())
            .or_else(|| {
                skipped_existing
                    .iter()
                    .find(|entry| {
                        entry.get("failure_key").and_then(Value::as_str) == Some(failure_key)
                    })
                    .and_then(|entry| {
                        entry
                            .get("task_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
            })
            .filter(|task_id| !task_id.is_empty())
    };
    let folded: Vec<Value> = folded_clusters
        .iter()
        .map(|&folded_index| {
            let cluster = &clusters[folded_index];
            let refs = refs_by_cluster[folded_index].clone().unwrap_or_default();
            let mut dependency_jobs: Vec<String> = Vec::new();
            let mut folded_into_jobs: Vec<String> = Vec::new();
            let mut folded_into_failure_keys: Vec<String> = Vec::new();
            for reference in &refs {
                if !dependency_jobs.contains(&reference.job_name) {
                    dependency_jobs.push(reference.job_name.clone());
                }
                if let Some(target) = sibling_cluster_for(sibling_index, reference) {
                    let sibling = &clusters[target];
                    if !folded_into_jobs.contains(&sibling.job) {
                        folded_into_jobs.push(sibling.job.clone());
                    }
                    if !folded_into_failure_keys.contains(&sibling.failure_key) {
                        folded_into_failure_keys.push(sibling.failure_key.clone());
                    }
                }
            }
            let covered_by_task_id = dependency_fold::fold_owner_indices(
                sibling_index,
                refs_by_cluster,
                folded_indices,
                folded_index,
            )
            .into_iter()
            .find_map(|owner| task_id_for_failure_key(&clusters[owner].failure_key))
            .unwrap_or_default();
            json!({
                "outcome": "folded",
                "reason": "dependency_only_failure",
                "match_kind": "dependency_fold",
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "workflow": cluster.workflow,
                "job": cluster.job,
                "jobs": cluster.jobs.iter().cloned().collect::<Vec<_>>(),
                "run_ids": cluster.run_ids(),
                "run_urls": cluster.run_urls(),
                "dependency_jobs": dependency_jobs,
                "folded_into_jobs": folded_into_jobs,
                "folded_into_failure_keys": folded_into_failure_keys,
                "task_id": covered_by_task_id,
                "covered_by_task_id": covered_by_task_id,
            })
        })
        .collect();
    let mut final_audit = pending_audit(
        filing_audit(audit, &filed, &skipped_existing),
        &pending_supersession,
    );
    final_audit["withheld_count"] = json!(withheld.len());
    final_audit["withheld"] = json!(withheld);
    final_audit["deferred_attribution"] = json!(deferred_attribution);
    final_audit["folded_count"] = json!(folded.len());
    final_audit["folded"] = json!(folded);
    Ok(json!({
        "outcome": OUTCOME_CURRENT_FAILURES,
        "capability": capability,
        "clusters": clusters.len(),
        "filed_count": filed.len(),
        "filed": filed,
        "pilot_candidate_count": pilot_candidates.len(),
        "pilot_candidates": pilot_candidates,
        "skipped_existing": skipped_existing,
        "withheld": withheld,
        "folded_count": folded.len(),
        "folded": folded,
        "repair_assessments": repair_assessments,
        "attributed": attributed,
        "deferred_attribution": deferred_attribution,
        "excluded_branch_failures": excluded_branch_failures,
        "skipped_over_cap": skipped_over_cap,
        "deferred_count": deferred.len(),
        "deferred": deferred,
        "evidence_incomplete": evidence_incomplete,
        "inconclusive": inconclusive,
        "infrastructure": infrastructure,
        "already_repaired": already_repaired,
        "pending_supersession": pending_supersession,
        "max_tasks": max_tasks,
        "audit": final_audit,
    }))
}
