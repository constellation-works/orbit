//! Land a task's delivery pull request through one call.
//!
//! The call merges the task's one open delivery PR at the head it has just
//! judged, then settles the task from that merge. The forge's branch protection
//! decides whether it merges: there is no bypass. Every condition that would
//! leave a merged PR unable to settle is checked before the merge, and a PR that
//! already merged settles without another request, so a retry after a lost reply
//! never merges twice.

use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use serde::Serialize;

use crate::OrbitRuntime;
use crate::application::task::TaskWorkspaceAutoPort;
use crate::runtime::task_pr_forge::{PullRequestBlockers, PullRequestMergeOutcome, TaskPrForge};

/// How long a landing waits while GitHub reports mergeability `UNKNOWN`.
///
/// A push to the base branch makes every open pull request `UNKNOWN` until
/// GitHub recomputes, often for 30–60 seconds on a busy integration branch.
/// Landing reads again after 5, 10, 20 and 40 seconds, then once more with
/// whatever of this budget remains, and then refuses with the same
/// `pull request #N mergeability is unknown` error. A reported conflict is
/// refused on the first read. The bound is fixed: the delay is GitHub's
/// recomputation, not an operator policy, so it is not a workspace setting.
const MERGEABILITY_UNKNOWN_BUDGET: Duration = Duration::from_secs(90);

/// Backoff before each further read, in seconds. The last read uses whatever
/// of [`MERGEABILITY_UNKNOWN_BUDGET`] remains after these steps.
const MERGEABILITY_UNKNOWN_BACKOFF_SECS: [u64; 4] = [5, 10, 20, 40];

/// Debug-build tests set this to `virtual` so the budget advances without
/// sleeping. Release builds ignore it, so a production binary cannot skip
/// the wait.
const MERGEABILITY_WAIT_TEST_ENV: &str = "ORBIT_TEST_MERGEABILITY_WAIT";

use super::candidate::Candidate;

/// The stable code a refused landing's error starts with.
pub const TASK_LANDING_REFUSED: &str = "task_landing_refused";

/// A landed delivery pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPullRequestLanding {
    /// The landed task.
    pub task_id: String,
    /// The merged pull request.
    pub pull_request: u64,
    /// The commit it merged as.
    pub merge_commit: String,
    /// The task's status after the call: `done`, or `review` when the merge
    /// has not settled yet.
    pub status: TaskStatus,
    /// Why the merged task did not settle in this call. The owner's next
    /// settlement pass completes it; merging again is neither needed nor possible.
    pub settlement_pending: Option<String>,
}

fn landing_refused(id: &str, reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "{TASK_LANDING_REFUSED}: task '{id}' did not land: {reason}"
    ))
}

impl OrbitRuntime {
    /// The PRs Orbit recorded for `task` that are still open.
    pub(crate) fn open_recorded_pull_requests(&self, task: &Task) -> Result<Vec<u64>, OrbitError> {
        let recorded = self.recorded_pull_requests(task);
        if recorded.is_empty() {
            return Ok(Vec::new());
        }
        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let mut open = Vec::new();
        for number in recorded {
            if forge.open_pull_request(&repo_root, number)?.is_some() {
                open.push(number);
            }
        }
        Ok(open)
    }

    /// Merge the delivery PR of the `review` task `id` and settle the task. A
    /// refusal, by Orbit or the forge, is a [`OrbitError::PolicyDenied`] starting
    /// with [`TASK_LANDING_REFUSED`]; any other error leaves the outcome unknown.
    pub fn land_task_pull_request(
        &self,
        id: &str,
        actor: Option<&str>,
    ) -> Result<TaskPullRequestLanding, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task = self.get_task(id)?;
        if task.status != TaskStatus::Review {
            return Err(landing_refused(
                id,
                format_args!("it is '{}'; only a task in review lands", task.status),
            ));
        }
        let carried = self.live_leaf_task_ids()?;
        let candidate = self
            .settlement_candidate(task, &carried)
            .map_err(|reason| landing_refused(id, reason))?;
        if let Some(handoff) = &candidate.revoked {
            return Err(landing_refused(id, handoff.handoff_landing_refusal()));
        }
        self.ensure_mutation_proposals_applied(&candidate.task)
            .map_err(|error| landing_refused(id, error))?;

        let Some(number) = self.open_delivery_pull_request(&candidate)? else {
            // Nothing is open: a merge this call lost the reply of, or one made
            // outside Orbit, settles here; otherwise this names why not.
            let settled = self
                .settle_externally_merged_task_as(id, actor)
                .map_err(|error| match error {
                    OrbitError::InvalidInput(reason) => landing_refused(id, reason),
                    error => error,
                })?;
            return Ok(TaskPullRequestLanding {
                task_id: settled.task_id,
                pull_request: settled.pr_number,
                merge_commit: settled.merge_commit,
                status: TaskStatus::Done,
                settlement_pending: None,
            });
        };
        let merge_commit = self.merge_delivery_pull_request(id, number)?;
        // The work has landed, so a settlement that fails now is not a failed
        // landing: the owner's settlement pass reads the same merge.
        let settlement_pending = self
            .settle_externally_merged_task_as(id, actor)
            .err()
            .map(|error| error.to_string());
        Ok(TaskPullRequestLanding {
            task_id: id.to_string(),
            pull_request: number,
            merge_commit,
            status: if settlement_pending.is_none() {
                TaskStatus::Done
            } else {
                TaskStatus::Review
            },
            settlement_pending,
        })
    }

    /// The candidate's one open PR that may deliver it, or `None` when none of
    /// its PRs is open. An open PR that cannot deliver the task, or more than
    /// one that can, is refused: the choice is not this call's to make.
    fn open_delivery_pull_request(&self, candidate: &Candidate) -> Result<Option<u64>, OrbitError> {
        let id = candidate.task.id.as_str();
        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let branches = &self
            .context
            .settings()
            .pr_settings()
            .conventions
            .branch_name;
        let mut open = Vec::new();
        for number in candidate.pr_numbers() {
            if let Some(pull) = forge.open_pull_request(&repo_root, number)? {
                open.push(pull);
            }
        }
        if open.is_empty() {
            return Ok(None);
        }
        let authors = self.settlement_authors(&repo_root);
        let mut delivering = Vec::new();
        for pull in open {
            let base = forge
                .lease_pull_request(&repo_root, &format!("#{}", pull.number))?
                .map(|bound| bound.base_branch);
            let delivers = orbit_types::task::names_task_delivery_pull_request(
                branches,
                &pull.head_branch,
                &pull.body,
                id,
            ) && base.as_deref().is_some_and(|base| {
                self.delivers_candidate(candidate, pull.number, &pull.author, base, &authors)
            });
            if !delivers {
                return Err(landing_refused(
                    id,
                    format_args!(
                        "open pull request #{} is not its delivery into its target branch",
                        pull.number
                    ),
                ));
            }
            delivering.push(pull.number);
        }
        match delivering.as_slice() {
            [number] => Ok(Some(*number)),
            numbers => Err(landing_refused(
                id,
                format_args!("it has several open delivery pull requests: {numbers:?}"),
            )),
        }
    }

    /// Merge `number` at the head whose required checks were just read.
    fn merge_delivery_pull_request(&self, id: &str, number: u64) -> Result<String, OrbitError> {
        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let blockers = await_known_merge_blockers(id, number, forge, &repo_root)?;
        if blockers.conflicting {
            return Err(landing_refused(
                id,
                format_args!("pull request #{number} conflicts with its base"),
            ));
        }
        if !blockers.failed_required_checks.is_empty()
            || !blockers.pending_required_checks.is_empty()
        {
            return Err(landing_refused(
                id,
                format_args!(
                    "pull request #{number} has required checks failed {:?} and unfinished {:?}",
                    blockers.failed_required_checks, blockers.pending_required_checks
                ),
            ));
        }
        match forge.merge_pull_request(&repo_root, number, &blockers.head_sha)? {
            PullRequestMergeOutcome::Merged { merge_commit } => Ok(merge_commit),
            PullRequestMergeOutcome::Refused { reason } => Err(landing_refused(
                id,
                format_args!("the forge refused to merge pull request #{number}: {reason}"),
            )),
        }
    }
}

/// Read what holds `number` back, waiting out GitHub's `UNKNOWN` mergeability
/// window. A conflict, a closed pull request and any other forge error return
/// on the first read.
fn await_known_merge_blockers(
    id: &str,
    number: u64,
    forge: &dyn TaskPrForge,
    repo_root: &std::path::Path,
) -> Result<PullRequestBlockers, OrbitError> {
    let started = Instant::now();
    let mut virtual_elapsed = mergeability_wait_is_virtual().then_some(Duration::ZERO);
    let mut attempt = 0_u32;
    loop {
        match forge.pull_request_blockers(repo_root, number) {
            Ok(Some(blockers)) => return Ok(blockers),
            Ok(None) => {
                return Err(landing_refused(
                    id,
                    format_args!("the forge cannot say what holds pull request #{number} back"),
                ));
            }
            Err(OrbitError::PullRequestMergeabilityUnknown { .. }) => {
                let elapsed = virtual_elapsed.unwrap_or_else(|| started.elapsed());
                let remaining = MERGEABILITY_UNKNOWN_BUDGET.saturating_sub(elapsed);
                // One more step than the documented backoff, so a clock that
                // does not advance cannot spin.
                if remaining.is_zero() || attempt > MERGEABILITY_UNKNOWN_BACKOFF_SECS.len() as u32 {
                    return Err(landing_refused(
                        id,
                        OrbitError::PullRequestMergeabilityUnknown { number },
                    ));
                }
                let delay = mergeability_backoff(attempt, remaining);
                attempt = attempt.saturating_add(1);
                if delay.is_zero() {
                    return Err(landing_refused(
                        id,
                        OrbitError::PullRequestMergeabilityUnknown { number },
                    ));
                }
                tracing::debug!(
                    target: "orbit.core.external_merge",
                    pull_request = number,
                    wait_secs = delay.as_secs(),
                    "landing waits while GitHub computes mergeability"
                );
                if let Some(elapsed) = virtual_elapsed.as_mut() {
                    *elapsed = elapsed.saturating_add(delay);
                } else {
                    std::thread::sleep(delay);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn mergeability_backoff(attempt: u32, remaining: Duration) -> Duration {
    let step = MERGEABILITY_UNKNOWN_BACKOFF_SECS
        .get(usize::try_from(attempt).unwrap_or(usize::MAX))
        .copied()
        .unwrap_or(40);
    Duration::from_secs(step).min(remaining)
}

fn mergeability_wait_is_virtual() -> bool {
    cfg!(debug_assertions) && std::env::var(MERGEABILITY_WAIT_TEST_ENV).as_deref() == Ok("virtual")
}
