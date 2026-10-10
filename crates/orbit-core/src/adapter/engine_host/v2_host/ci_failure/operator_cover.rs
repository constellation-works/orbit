//! Ownership retained by an operator's archive or rejection: the exact failure
//! key, plus the coordinate-free diagnostic set for compiler clusters.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskRelationType, TaskStatus, is_valid_orb_task_id};
use orbit_types::workflow::LandingObservationStatus;
use serde_json::{Value, json};

use super::cluster::{FailureCluster, task_open_compiler_identity};
use super::filing::{CI_FAILURE_KEY_TAG_PREFIX, CI_FAILURE_TAG};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::DuplicateTaskLookup;
use crate::application::automation::source::Source;

pub(super) struct OperatorAssessment {
    pub(super) withheld: Option<Value>,
    pub(super) failed_covers: Vec<Value>,
}

enum CoverState {
    Open,
    Landed(String),
    Closed,
    Unavailable,
}

pub(super) struct OperatorCovers<'a> {
    runtime: &'a OrbitRuntime,
    source: Source<'a>,
    now: DateTime<Utc>,
    suppression: Duration,
    covers: BTreeMap<String, CoverState>,
}

impl<'a> OperatorCovers<'a> {
    pub(super) fn new(runtime: &'a OrbitRuntime) -> Result<Self, OrbitError> {
        let config = orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
            runtime.global_root(),
            runtime.shared_root(),
        ))?;
        let hours = config
            .value_for("ci_failure.operator_suppression_hours")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| {
                OrbitError::InvalidInput("CI operator suppression setting unavailable".into())
            })?;
        Ok(Self {
            runtime,
            source: Source::new(&runtime.paths().repo_root),
            now: runtime.ci_failure_time.unwrap_or_else(Utc::now),
            suppression: Duration::hours(hours as i64),
            covers: BTreeMap::new(),
        })
    }

    pub(super) fn assess<L: DuplicateTaskLookup + ?Sized>(
        &mut self,
        cluster: &FailureCluster,
        lookup: &L,
    ) -> Result<OperatorAssessment, OrbitError> {
        let mut owners = BTreeMap::new();
        for key in std::iter::once(&cluster.failure_key).chain(&cluster.legacy_keys) {
            for task in lookup.list_tasks_by_tags(&[format!("{CI_FAILURE_KEY_TAG_PREFIX}{key}")])? {
                if matches!(task.status, TaskStatus::Archived | TaskStatus::Rejected)
                    && task.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                {
                    // Old compiler keys can collide on chatter. Retain the
                    // same supplying-evidence requirement as open-owner lookup.
                    if cluster.compiler_cause.is_some()
                        && key != &cluster.failure_key
                        && !cluster.runs.iter().any(|run| {
                            super::grouping::legacy_source_matches(&task.description, run)
                        })
                    {
                        continue;
                    }
                    owners.insert(task.id.clone(), task);
                }
            }
        }
        // A compiler failure key embeds the observed checkout, so a hold taken
        // at one commit is invisible to the exact-key lookup at the next. Match
        // closed CI-sweep owners on the same location-free diagnostic set the
        // open-owner lookup uses; the exact key stays the reported evidence.
        if let Some(identity) = cluster.open_compiler_identity() {
            for task in lookup.list_tasks()?.iter() {
                if matches!(task.status, TaskStatus::Archived | TaskStatus::Rejected)
                    && task.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                    && task_open_compiler_identity(task).as_ref() == Some(&identity)
                {
                    owners
                        .entry(task.id.clone())
                        .or_insert_with(|| task.clone());
                }
            }
        }
        let mut assessment = OperatorAssessment {
            withheld: None,
            failed_covers: Vec::new(),
        };
        for owner in owners.values() {
            let covers = owner
                .relations
                .iter()
                .filter(|relation| relation.relation_type == TaskRelationType::CoveredBy)
                .collect::<Vec<_>>();
            if covers.is_empty() {
                // Metadata edits after an archive must not restart the window.
                let archived_at = self
                    .runtime
                    .get_task_history(&owner.id)?
                    .iter()
                    .rev()
                    .find(|event| event.to_status == Some(owner.status))
                    .map(|event| event.at)
                    .unwrap_or(owner.updated_at);
                if self.now < archived_at + self.suppression {
                    assessment.withheld = Some(json!({
                        "outcome": "withheld", "reason": "operator_archived",
                        "task_id": owner.id, "owner": owner.id,
                        "failure_key": cluster.failure_key, "cluster_key": cluster.cluster_key,
                        "status": owner.status, "until": archived_at + self.suppression,
                    }));
                    return Ok(assessment);
                }
                continue;
            }
            for relation in covers {
                let target = &relation.target;
                if !self.covers.contains_key(target) {
                    // Bound forge and delivery reads, failing closed per key.
                    let state = if self.covers.len() >= 16 {
                        CoverState::Unavailable
                    } else {
                        self.read_cover(target, lookup)
                    };
                    self.covers.insert(target.clone(), state);
                }
                let (reason, landed) = match &self.covers[target] {
                    CoverState::Closed => continue,
                    CoverState::Open => ("operator_cover_open", None),
                    CoverState::Unavailable => ("operator_cover_unavailable", None),
                    CoverState::Landed(commit) => {
                        if self
                            .source
                            .git(&[
                                "merge-base",
                                "--is-ancestor",
                                commit,
                                &cluster.tested_commit,
                            ])
                            .is_ok()
                        {
                            assessment.failed_covers.push(json!({
                                "task_id": owner.id, "cover": target, "landed_commit": commit,
                                "reason": "cover_did_not_hold", "failure_key": cluster.failure_key,
                            }));
                            continue;
                        }
                        ("awaiting_cover_commit", Some(commit))
                    }
                };
                assessment.withheld = Some(json!({
                    "outcome": "covered", "reason": reason,
                    "task_id": owner.id, "owner": owner.id, "cover": target,
                    "landed_commit": landed,
                    "failure_key": cluster.failure_key, "cluster_key": cluster.cluster_key,
                }));
                return Ok(assessment);
            }
        }
        Ok(assessment)
    }

    fn read_cover<L: DuplicateTaskLookup + ?Sized>(&self, target: &str, lookup: &L) -> CoverState {
        if is_valid_orb_task_id(target) {
            let Ok(task) = lookup.get_task(target) else {
                return CoverState::Unavailable;
            };
            return match task.status {
                TaskStatus::Done => self
                    .runtime
                    .observe_task_delivery(target, None)
                    .ok()
                    .filter(|delivery| delivery.landing.status == LandingObservationStatus::Merged)
                    .and_then(|delivery| delivery.landing.landed_commit)
                    .filter(|commit| full_revision(commit))
                    .map(CoverState::Landed)
                    .unwrap_or(CoverState::Unavailable),
                TaskStatus::Archived | TaskStatus::Rejected => CoverState::Closed,
                _ => CoverState::Open,
            };
        }
        let Some(reference) = target.strip_prefix("github-pr:") else {
            return CoverState::Unavailable;
        };
        let Ok(pr) = self.source.ci_cover_pull_request(reference) else {
            return CoverState::Unavailable;
        };
        match pr["state"].as_str() {
            Some("OPEN") => CoverState::Open,
            Some("CLOSED") => CoverState::Closed,
            Some("MERGED") => pr["mergeCommit"]["oid"]
                .as_str()
                .filter(|commit| full_revision(commit))
                .map(|commit| CoverState::Landed(commit.into()))
                .unwrap_or(CoverState::Unavailable),
            _ => CoverState::Unavailable,
        }
    }
}

fn full_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
