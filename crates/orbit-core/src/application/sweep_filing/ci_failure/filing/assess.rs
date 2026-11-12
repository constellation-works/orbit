//! Read-only assessment: every duplicate lookup, operator cover, repair
//! assessment and landed/descendant repair, completed before the first task
//! write so a transient lookup failure leaves no partial filing.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::sweep_filing::bounded_error;
use crate::application::sweep_filing::duplicate_tasks::{
    DuplicateMatchKind, DuplicateTaskLookup, DuplicateTaskMatch, SnapshotDuplicateLookup,
};

use super::super::cluster::FailureCluster;
use super::super::evidence::{retryable_pipeline_error, single_retryable_error};
use super::super::identity::CiTaskIdentities;
use super::super::landed_repair::LandedRepairs;
use super::super::operator_cover::{OperatorAssessment, OperatorCovers};
use super::super::repair_assessment;

/// Per-cluster owners, index-aligned with the clusters they assess.
pub(super) struct ClusterOwners {
    pub(super) operator_assessments: Vec<OperatorAssessment>,
    pub(super) duplicate_matches: Vec<Option<DuplicateTaskMatch>>,
    pub(super) duplicate_tasks: Vec<Option<Task>>,
    pub(super) descendant_landings: Vec<Option<Value>>,
}

pub(super) struct ClusterAssessments {
    pub(super) owners: ClusterOwners,
    pub(super) repair_assessments: Vec<Value>,
}

pub(super) fn assess_clusters<L>(
    runtime: &OrbitRuntime,
    lookup: &L,
    clusters: &[FailureCluster],
    audit: &Value,
) -> Result<ClusterAssessments, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    // Complete every external lookup before the first task write. A transient
    // duplicate-check failure must leave no partial filing or dedupe state.
    // Every cluster and legacy key is assessed against one snapshot: the
    // task list is hydrated at most once per filing and each open task's
    // comments are read at most once, however many clusters the run has.
    let lookup = SnapshotDuplicateLookup::new(lookup);
    let identities = CiTaskIdentities::new(runtime);
    // Each cluster otherwise pays a store query per failure and legacy
    // key for operator, exact and legacy coverage. Past this many keys
    // one hydrated listing is cheaper, and the snapshot then answers
    // every tag selection from it.
    let tag_queries: usize = clusters
        .iter()
        .map(|cluster| 2 + cluster.legacy_keys.len())
        .sum();
    if tag_queries >= 16 {
        lookup.list_tasks()?;
    }
    let mut operator_covers = OperatorCovers::new(runtime)?;
    let operator_assessments = clusters
        .iter()
        .map(|cluster| operator_covers.assess(cluster, &lookup, &identities))
        .collect::<Result<Vec<_>, _>>()?;
    let mut assessor = repair_assessment::Assessor::new(runtime);
    let mut repair_assessments = Vec::new();
    let duplicate_matches = clusters
        .iter()
        .zip(&operator_assessments)
        .map(|(cluster, operator)| {
            if operator.withheld.is_some() {
                return Ok(None);
            }
            let mut existing =
                cluster
                    .find_covering_task(&lookup, &identities)
                    .map_err(|error| {
                        retryable_pipeline_error(
                            "dedupe_lookup",
                            audit,
                            vec![single_retryable_error(
                                "registration",
                                "find_covering_task",
                                json!({"failure_key": cluster.failure_key}),
                                &bounded_error(&error.to_string()),
                            )],
                        )
                    })?;
            // Only the disproven cover (or its owner) loses coverage. A
            // later completed repair remains a valid duplicate owner.
            if existing.as_ref().is_some_and(|matched| {
                operator.failed_covers.iter().any(|cover| {
                    cover["cover"].as_str() == Some(matched.task_id.as_str())
                        || cover["task_id"].as_str() == Some(matched.task_id.as_str())
                }) && lookup
                    .get_task(&matched.task_id)
                    .is_ok_and(|task| task.status == TaskStatus::Done)
            }) {
                existing = None;
            }
            if existing.is_some() {
                return Ok(existing);
            }
            let assessment = assessor.assess(cluster, &lookup);
            if !assessment.evidence.is_null() {
                repair_assessments.push(assessment.evidence.clone());
            }
            Ok(assessment.owner.map(|task_id| DuplicateTaskMatch {
                task_id,
                match_kind: DuplicateMatchKind::CoveredByRepair,
                evidence: assessment.evidence,
            }))
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;
    // A failure with no open owner may still predate a completed repair.
    let mut landed_repairs = LandedRepairs::new(runtime);
    let descendant_landings = clusters
        .iter()
        .zip(&duplicate_matches)
        .zip(&operator_assessments)
        .map(|((cluster, duplicate_match), operator)| {
            if duplicate_match.is_some() || operator.withheld.is_some() {
                return Ok(None);
            }
            landed_repairs
                .find(cluster, &lookup, &identities)
                .map_err(|error| {
                    retryable_pipeline_error(
                        "dedupe_lookup",
                        audit,
                        vec![single_retryable_error(
                            "registration",
                            "find_landed_repair",
                            json!({"failure_key": cluster.failure_key}),
                            &bounded_error(&error.to_string()),
                        )],
                    )
                })
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;
    let duplicate_tasks = duplicate_matches
        .iter()
        .map(|duplicate_match| {
            duplicate_match
                .as_ref()
                .map(|matched| {
                    runtime.get_task(&matched.task_id).map_err(|error| {
                        retryable_pipeline_error(
                            "pilot_candidate_lookup",
                            audit,
                            vec![single_retryable_error(
                                "registration",
                                "reload_duplicate_task",
                                json!({"task_id": matched.task_id}),
                                &bounded_error(&error.to_string()),
                            )],
                        )
                    })
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ClusterAssessments {
        owners: ClusterOwners {
            operator_assessments,
            duplicate_matches,
            duplicate_tasks,
            descendant_landings,
        },
        repair_assessments,
    })
}
