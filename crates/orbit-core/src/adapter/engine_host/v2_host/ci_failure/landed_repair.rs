//! A failure observed before its repair landed.
//!
//! Dedupe only considers open owners, so once a sweep repair is done a red run
//! that completes late on an older commit would be filed again. When a done
//! sweep task with the same normalized signature (or exact failure key) landed
//! at a descendant of the commit the runner tested, that failure is deferred
//! instead: newer runs of the branch already test the repair, and a later
//! sweep files only if the failure reproduces on one of them.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::LandingObservationStatus;
use serde_json::{Value, json};

use super::cluster::FailureCluster;
use super::filing::{CI_FAILURE_KEY_TAG_PREFIX, CI_FAILURE_TAG};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::DuplicateTaskLookup;
use crate::application::automation::source::Source;

/// Why a cluster was deferred rather than filed.
pub(super) const DESCENDANT_LANDING_REASON: &str = "repaired_by_descendant_landing";
/// Only repairs completed this recently are assessed.
const LOOKBACK_DAYS: i64 = 30;
/// Delivery observations read per filing; each is a run-store query.
const MAX_DELIVERY_READS: usize = 16;

pub(super) struct LandedRepairs<'a> {
    runtime: &'a OrbitRuntime,
    source: Source<'a>,
    remaining: usize,
}

impl<'a> LandedRepairs<'a> {
    pub(super) fn new(runtime: &'a OrbitRuntime) -> Self {
        Self {
            runtime,
            source: Source::new(&runtime.paths().repo_root),
            remaining: MAX_DELIVERY_READS,
        }
    }

    /// The pending-supersession entry for `cluster` when a matching done
    /// repair landed at a descendant of its tested commit.
    pub(super) fn find<L: DuplicateTaskLookup + ?Sized>(
        &mut self,
        cluster: &FailureCluster,
        lookup: &L,
    ) -> Result<Option<Value>, OrbitError> {
        if !full_revision(&cluster.tested_commit) {
            return Ok(None);
        }
        let tasks = lookup.list_tasks()?;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(LOOKBACK_DAYS);
        let mut candidates = tasks
            .iter()
            .filter(|task| {
                task.status == TaskStatus::Done
                    && task.updated_at >= cutoff
                    && task.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                    && same_root_cause(task, cluster)
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
        for task in candidates {
            if self.remaining == 0 {
                return Ok(None);
            }
            self.remaining -= 1;
            let Some(landed) = self.landed_commit(task) else {
                continue;
            };
            if landed == cluster.tested_commit || !self.descends(&cluster.tested_commit, &landed) {
                continue;
            }
            return Ok(Some(pending_entry(cluster, task, &landed)));
        }
        Ok(None)
    }

    fn landed_commit(&self, task: &Task) -> Option<String> {
        let observation = self.runtime.observe_task_delivery(&task.id, None).ok()?;
        (observation.landing.status == LandingObservationStatus::Merged)
            .then_some(observation.landing.landed_commit)
            .flatten()
            .filter(|commit| full_revision(commit))
    }

    fn descends(&self, ancestor: &str, descendant: &str) -> bool {
        self.source
            .git(&["merge-base", "--is-ancestor", ancestor, descendant])
            .is_ok()
    }
}

fn same_root_cause(task: &Task, cluster: &FailureCluster) -> bool {
    let key_tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key);
    task.tags.contains(&key_tag)
        || cluster.signature_line().is_some_and(|expected| {
            task.description
                .lines()
                .any(|line| line.trim_end() == expected)
        })
}

fn pending_entry(cluster: &FailureCluster, task: &Task, landed: &str) -> Value {
    json!({
        "reason": DESCENDANT_LANDING_REASON,
        "task_id": task.id,
        "landed_commit": landed,
        "failure_key": cluster.failure_key,
        "cluster_key": cluster.cluster_key,
        "workflow": cluster.workflow,
        "job": cluster.job,
        "step": cluster.step,
        "tested_commit": cluster.tested_commit,
        "run_ids": cluster.run_ids(),
        "run_urls": cluster.run_urls(),
        "evidence": format!(
            "a completed repair of the same root cause landed at {landed}, a descendant of the \
             tested commit {}; this failure is filed only if it reproduces on the newest \
             completed run",
            cluster.tested_commit
        ),
    })
}

fn full_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
