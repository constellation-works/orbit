use super::pass_types::TASK_LEAF_SCAN;
use super::{PassReads, RepairAuthority, RepairObservation, Verdict};
use crate::OrbitRuntime;
use crate::application::job::orphan_adoption::AdoptionAuthority;
use crate::application::job::resume::OUT_OF_RUN_AUTO_FIX_KEY;
use crate::runtime::run_input::carried_task_ids;
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery, TaskListFilter};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{self, JobRun, JobRunState};
use serde_json::{Value, json};

impl OrbitRuntime {
    /// Start an out-of-run auto-fix for each blocked Orbit pull request whose
    /// task has no live run, at most `budget` of them, within `authority`.
    /// Returns the repairs started and the candidates left alone, and the
    /// pass's own error when it could not read its candidates. Never fails.
    pub(crate) fn repair_out_of_run_pull_requests(
        &self,
        authority: &AdoptionAuthority<'_>,
        budget: usize,
    ) -> Value {
        if !authority.auto_fix {
            return json!({ "started": [], "skipped": [] });
        }
        if self.is_write_free() {
            return json!({ "started": [], "skipped": [], "hold_reason": "write_free" });
        }
        let throttled = self.resource_admission().throttle.is_some();
        let repair_authority = RepairAuthority {
            drain_run_id: Some(authority.drain_run_id),
            actor: Some(authority.drain_run_id),
            completion_done: authority.completion_done,
        };
        let tasks = {
            let _span = tracing::debug_span!("pr_repair_candidates").entered();
            // A repair needs a stopped or held delivery. Project task IDs
            // from all such leaves before reading envelopes, so unrelated task history
            // costs no reads and old stopped leaves remain discoverable. This
            // projection is fresh on each pass, with no age window or cache.
            let tasks = self
                .stores()
                .jobs()
                .job_run_task_ids(&JobRunQuery {
                    job_id: Some(workflow::TASK_PR_PIPELINE_JOB.to_string()),
                    states: vec![
                        JobRunState::Failed,
                        JobRunState::Timeout,
                        JobRunState::Interrupted,
                        JobRunState::Held,
                    ],
                    include_steps: false,
                    ..Default::default()
                })
                .and_then(|mut ids| {
                    // Malformed bindings in an unrelated historical run cannot
                    // name a task, and must not refuse the other candidates.
                    ids.retain(|id| orbit_types::task::is_valid_orb_task_id(id));
                    // Narrow by task state before reading envelopes: only a
                    // task now in review/blocked/backlog can be repaired, so
                    // intersect the projected ids with that bounded status
                    // population (answered from the generated index alone)
                    // and request metadata for at most those ids. A worker
                    // reads through its owner, which has no local index, so
                    // it keeps the keyed owner read.
                    if self.worker_invocation().is_none() {
                        let open_filter = TaskListFilter {
                            statuses: Some(vec![
                                TaskStatus::Review,
                                TaskStatus::Blocked,
                                TaskStatus::Backlog,
                            ]),
                            ..TaskListFilter::default()
                        };
                        match self.task_candidate_keys(&open_filter, usize::MAX) {
                            Ok(Some(keys)) => {
                                let open: std::collections::BTreeSet<String> =
                                    keys.items.into_iter().map(|key| key.id).collect();
                                ids.retain(|id| open.contains(id));
                            }
                            Ok(None) => {
                                // The index cannot serve this filter: read
                                // envelopes for the open statuses only (the
                                // bounded side) and keep the projected ones.
                                let candidates = self.task_candidates(&open_filter, usize::MAX)?;
                                return Ok(candidates
                                    .items
                                    .into_iter()
                                    .filter(|envelope| ids.contains(&envelope.id))
                                    .map(crate::application::task::metadata_task)
                                    .collect());
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    self.list_task_metadata_for_ids(&ids)
                });
            match tasks {
                Ok(tasks) => tasks,
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.drain",
                        error = %error,
                        "could not list tasks; this pass repairs no pull request",
                    );
                    return json!({
                        "started": [],
                        "skipped": [],
                        "pass_error": format!("list tasks: {error}"),
                    });
                }
            }
        };
        let mut started = Vec::new();
        let mut skipped = Vec::new();
        let mut reads = PassReads::default();
        // Re-read the newest own leaf, including live and finished ones, so a
        // newer delivery supersedes the stopped leaf that nominated this task.
        let leaf_span = tracing::debug_span!("pr_repair_leaf_lookup").entered();
        let mut leaves = Vec::new();
        for task in tasks.iter().filter(|task| {
            matches!(
                task.status,
                TaskStatus::Review | TaskStatus::Blocked | TaskStatus::Backlog
            ) && task.github_pr_number().is_some()
        }) {
            match self.newest_own_delivery_leaf(&task.id) {
                Ok(Some(run)) => leaves.push((task.id.clone(), run)),
                Ok(None) => {}
                Err(error) => skipped.push(json!({
                    "task_id": task.id,
                    "source_run_id": Value::Null,
                    "pr_number": task.github_pr_number(),
                    "blocker": Value::Null,
                    "hold_code": Value::Null,
                    "reason": "store_unreadable",
                    "detail": format!("read the task's delivery leaves: {error}"),
                })),
            }
        }
        drop(leaf_span);
        // Newest first, so a pass short of slots serves the freshest leaves.
        leaves.sort_by(|(left_task, left), (right_task, right)| {
            right
                .recency_at()
                .cmp(&left.recency_at())
                .then_with(|| left.run_id.cmp(&right.run_id))
                .then_with(|| left_task.cmp(right_task))
        });
        for (task_id, run) in leaves {
            let task_id = task_id.as_str();
            let hold = if throttled {
                Some((
                    "resource_throttled",
                    "host resource pressure holds this pass",
                ))
            } else if started.len() >= budget {
                Some((
                    "no_capacity",
                    "no free leaf slot remains after adoption and earlier repairs",
                ))
            } else {
                None
            };
            let mut observation = RepairObservation::default();
            let verdict = match self.repair_verdict(
                &run,
                task_id,
                &repair_authority,
                &mut reads,
                &mut observation,
                hold,
            ) {
                Verdict::Repair(repair) => match self.plan_repair(&run, *repair, &mut reads) {
                    Verdict::Repair(repair) => self.start_repair(&run, &repair_authority, *repair),
                    verdict => verdict,
                },
                verdict => verdict,
            };
            match verdict {
                Verdict::Ignore(..) | Verdict::Repair(_) => {}
                Verdict::Started(entry) => started.push(entry),
                Verdict::Skip(reason, detail) => {
                    tracing::debug!(
                        target: "orbit.core.drain",
                        run_id = %run.run_id,
                        task_id = %task_id,
                        reason,
                        detail = %detail,
                        "drain left a task's pull request to its own delivery",
                    );
                    skipped.push(json!({
                        "task_id": task_id,
                        "source_run_id": run.run_id,
                        "pr_number": observation.pr_number,
                        "blocker": observation.blocker,
                        "hold_code": observation.hold_code,
                        "reason": reason,
                        "detail": detail,
                    }));
                }
            }
        }
        json!({ "started": started, "skipped": skipped })
    }

    /// The newest `task_pr_pipeline` run that carries only `task_id`, whatever
    /// its state: a later finished run speaks for the task, so the verdict
    /// never resumes an older stopped one.
    pub(in crate::application::job::pr_repair) fn newest_own_delivery_leaf(
        &self,
        task_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(workflow::TASK_PR_PIPELINE_JOB.to_string()),
                task_id: Some(task_id.to_string()),
                order_by: JobRunOrder::Recency,
                limit: Some(TASK_LEAF_SCAN),
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .into_iter()
            .find(|run| {
                let carried = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
                carried.len() == 1 && carried.contains(task_id)
            }))
    }

    /// The out-of-run auto-fixes live now, for readiness: each pending or
    /// running delivery leaf a drain resumed to repair its pull request.
    pub(crate) fn live_out_of_run_auto_fixes(&self) -> Result<Vec<Value>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_pending_or_running_job_runs(workflow::TASK_PR_PIPELINE_JOB)?
            .into_iter()
            .filter_map(|run| {
                let input = run.input.as_ref()?;
                let repair = input.get(OUT_OF_RUN_AUTO_FIX_KEY)?;
                Some(json!({
                    "run_id": run.run_id,
                    "state": run.state,
                    "task_ids": carried_task_ids(input),
                    "pr_number": repair["pr_number"],
                    "blocker": repair["blocker"],
                    "hold_code": repair.get("hold_code").cloned().unwrap_or(Value::Null),
                    "head_sha": repair["head_sha"],
                    "source_run_id": repair["source_run_id"],
                    "drain_run_id": repair["drain_run_id"],
                }))
            })
            .collect())
    }
}
