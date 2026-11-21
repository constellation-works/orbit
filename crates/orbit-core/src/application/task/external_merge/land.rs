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

/// The audit step a merge requested through `orbit.workflow.land` names.
const LAND_TOOL_MERGE_STEP: &str = "workflow.land";

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

/// The open delivery pull requests a task has.
pub(super) enum OpenDelivery {
    /// None of the task's recorded pull requests is open.
    None,
    /// Exactly one open pull request delivers the task into its target branch.
    One { number: u64, target_branch: String },
    /// Several open pull requests deliver the task, ascending. Which one lands
    /// is not Orbit's choice.
    Several(Vec<u64>),
}

/// Outcome of checking a delivery pull request a second time and asking the
/// forge to merge it.
pub(super) enum DeliveryMergeOutcome {
    /// The forge merged the pull request.
    Merged { merge_commit: String },
    /// Mergeability could not be read (`None` or `PullRequestMergeabilityUnknown`).
    BlockersUnavailable { reason: String },
    /// The pull request head moved from the judged `expected_head`.
    HeadChanged,
    /// The pull request conflicts with its base.
    Conflicting,
    /// Required checks are no longer ready (`failed`, `pending`, or absent
    /// passed checks when `require_passed_check` is set).
    ChecksNotReady {
        failed: Vec<String>,
        pending: Vec<String>,
    },
    /// The pull request no longer targets `expected_base`.
    TargetBranchChanged { reason: String },
    /// The forge itself refused the merge request.
    ForgeRefused { reason: String },
}

impl DeliveryMergeOutcome {
    fn into_merge_commit(self, id: &str, number: u64) -> Result<String, OrbitError> {
        match self {
            Self::Merged { merge_commit } => Ok(merge_commit),
            Self::BlockersUnavailable { reason } | Self::TargetBranchChanged { reason } => {
                Err(landing_refused(id, reason))
            }
            Self::HeadChanged => Err(landing_refused(
                id,
                format_args!("pull request #{number} head changed since it was judged"),
            )),
            Self::Conflicting => Err(landing_refused(
                id,
                format_args!("pull request #{number} conflicts with its base"),
            )),
            Self::ChecksNotReady { failed, pending } => Err(landing_refused(
                id,
                format_args!(
                    "pull request #{number} has required checks failed {failed:?} and unfinished {pending:?}"
                ),
            )),
            Self::ForgeRefused { reason } => Err(landing_refused(
                id,
                format_args!("the forge refused to merge pull request #{number}: {reason}"),
            )),
        }
    }
}

pub(super) fn landing_refused(id: &str, reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "{TASK_LANDING_REFUSED}: task '{id}' did not land: {reason}"
    ))
}

/// Refuse landing when `observed_base` does not match the `expected_base`
/// established during candidate selection.
pub(super) fn ensure_delivery_target_branch(
    number: u64,
    observed_base: Option<&str>,
    expected_base: &str,
) -> Result<(), String> {
    if observed_base == Some(expected_base) {
        return Ok(());
    }
    Err(format!(
        "pull request #{number} targets {}, not its delivery target branch {expected_base}",
        observed_base.unwrap_or("no branch")
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
    /// with `TASK_LANDING_REFUSED`; any other error leaves the outcome unknown.
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

        let (number, expected_base) = match self.open_delivery_pull_request(&candidate)? {
            OpenDelivery::One {
                number,
                target_branch,
            } => (number, target_branch),
            OpenDelivery::Several(numbers) => {
                return Err(landing_refused(
                    id,
                    format_args!("it has several open delivery pull requests: {numbers:?}"),
                ));
            }
            OpenDelivery::None => {
                // Nothing is open: a merge this call lost the reply of, or one made
                // outside Orbit, settles here; otherwise this names why not.
                let settled = self.settle_externally_merged_task_as(id, actor).map_err(
                    |error| match error {
                        OrbitError::InvalidInput(reason) => landing_refused(id, reason),
                        error => error,
                    },
                )?;
                return Ok(TaskPullRequestLanding {
                    task_id: settled.task_id,
                    pull_request: settled.pr_number,
                    merge_commit: settled.merge_commit,
                    status: TaskStatus::Done,
                    settlement_pending: None,
                });
            }
        };
        let merge_commit = self
            .merge_delivery_pull_request(
                id,
                number,
                None,
                &expected_base,
                false,
                &super::PullRequestMergeAttribution {
                    run_id: None,
                    step: LAND_TOOL_MERGE_STEP,
                    task_id: Some(id),
                },
            )?
            .into_merge_commit(id, number)?;
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

    /// Refuse anything that would prevent a blocked handoff from settling after merge.
    pub(in crate::application::task) fn preflight_handoff_landing(
        &self,
        task: &Task,
        pull: &crate::runtime::task_pr_forge::HandoffLandingPullRequest,
    ) -> Result<(), OrbitError> {
        let candidate = self
            .settlement_candidate(task.clone(), &self.live_leaf_task_ids()?)
            .map_err(|reason| landing_refused(&task.id, reason))?;
        if candidate.revoked.is_some() {
            return Err(landing_refused(
                &task.id,
                "an unsettled handoff claim owns completion",
            ));
        }
        let pr = &pull.blockers.pull;
        let authors = self.settlement_authors(&self.context.paths().repo_root);
        if !candidate.pr_numbers().contains(&pr.number)
            || !orbit_types::task::names_task_delivery_pull_request(
                &self
                    .context
                    .settings()
                    .pr_settings()
                    .conventions
                    .branch_name,
                &pr.head_branch,
                &pr.body,
                &task.id,
            )
            || !self.delivers_candidate(
                &candidate,
                pr.number,
                &pr.author,
                &pull.base_branch,
                &authors,
            )
        {
            return Err(landing_refused(
                &task.id,
                "the handoff pull request is not its delivery into its target branch",
            ));
        }
        self.ensure_mutation_proposals_applied(task)?;
        self.ensure_resolves_are_workspace_local(task)?;
        Ok(())
    }

    /// The candidate's open PRs that may deliver it. An open PR that cannot
    /// deliver the task is refused; several that can are reported as
    /// [`OpenDelivery::Several`], because the choice is not this call's to make.
    pub(super) fn open_delivery_pull_request(
        &self,
        candidate: &Candidate,
    ) -> Result<OpenDelivery, OrbitError> {
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
            return Ok(OpenDelivery::None);
        }
        let authors = self.settlement_authors(&repo_root);
        let mut delivering = Vec::new();
        for pull in open {
            let base = forge
                .lease_pull_request(&repo_root, &format!("#{}", pull.number))?
                .map(|bound| bound.base_branch);
            let target_branch = base.filter(|base| {
                orbit_types::task::names_task_delivery_pull_request(
                    branches,
                    &pull.head_branch,
                    &pull.body,
                    id,
                ) && self.delivers_candidate(candidate, pull.number, &pull.author, base, &authors)
            });
            let Some(target_branch) = target_branch else {
                return Err(landing_refused(
                    id,
                    format_args!(
                        "open pull request #{} is not its delivery into its target branch",
                        pull.number
                    ),
                ));
            };
            delivering.push((pull.number, target_branch));
        }
        delivering.sort_unstable_by_key(|(number, _)| *number);
        let mut iter = delivering.into_iter();
        Ok(match (iter.next(), iter.next()) {
            (Some((number, target_branch)), None) => OpenDelivery::One {
                number,
                target_branch,
            },
            (Some((first, _)), Some((second, _))) => OpenDelivery::Several(
                [first, second]
                    .into_iter()
                    .chain(iter.map(|(number, _)| number))
                    .collect(),
            ),
            (None, _) => OpenDelivery::None,
        })
    }

    /// Merge `number` at the head whose required checks were just read,
    /// verifying it still targets `expected_base`.
    /// `expected_head` pins the head a caller already judged (the owner's
    /// automatic landing); a pull request that moved since returns
    /// [`DeliveryMergeOutcome::HeadChanged`] without issuing a merge.
    pub(super) fn merge_delivery_pull_request(
        &self,
        id: &str,
        number: u64,
        expected_head: Option<&str>,
        expected_base: &str,
        require_passed_check: bool,
        attribution: &super::PullRequestMergeAttribution<'_>,
    ) -> Result<DeliveryMergeOutcome, OrbitError> {
        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let blockers = match await_known_merge_blockers(number, forge, &repo_root)? {
            KnownMergeBlockers::Ready(blockers) => blockers,
            KnownMergeBlockers::Unavailable(reason) => {
                return Ok(DeliveryMergeOutcome::BlockersUnavailable { reason });
            }
        };
        if expected_head.is_some_and(|head| head != blockers.head_sha) {
            return Ok(DeliveryMergeOutcome::HeadChanged);
        }
        if blockers.conflicting {
            return Ok(DeliveryMergeOutcome::Conflicting);
        }
        if !blockers.failed_required_checks.is_empty()
            || !blockers.pending_required_checks.is_empty()
            || (require_passed_check && blockers.passed_required_checks.is_empty())
        {
            return Ok(DeliveryMergeOutcome::ChecksNotReady {
                failed: blockers.failed_required_checks,
                pending: blockers.pending_required_checks,
            });
        }
        let base = forge
            .lease_pull_request(&repo_root, &format!("#{number}"))?
            .map(|bound| bound.base_branch);
        if let Err(reason) = ensure_delivery_target_branch(number, base.as_deref(), expected_base) {
            return Ok(DeliveryMergeOutcome::TargetBranchChanged { reason });
        }
        let message = orbit_engine::compose_squash_merge_message(
            &repo_root,
            std::slice::from_ref(&self.get_task(id)?),
            self.pr_config(),
            base.as_deref(),
            &blockers.head_sha,
            number,
        );
        match super::audited_forge_merge(
            self,
            forge,
            &repo_root,
            number,
            &blockers.head_sha,
            &message,
            attribution,
        )? {
            PullRequestMergeOutcome::Merged { merge_commit } => {
                Ok(DeliveryMergeOutcome::Merged { merge_commit })
            }
            PullRequestMergeOutcome::Refused { reason } => {
                Ok(DeliveryMergeOutcome::ForgeRefused { reason })
            }
        }
    }
}

enum KnownMergeBlockers {
    Ready(PullRequestBlockers),
    Unavailable(String),
}

/// Read what holds `number` back, waiting out GitHub's `UNKNOWN` mergeability
/// window. A conflict, a closed pull request and any other forge error return
/// on the first read.
fn await_known_merge_blockers(
    number: u64,
    forge: &dyn TaskPrForge,
    repo_root: &std::path::Path,
) -> Result<KnownMergeBlockers, OrbitError> {
    let started = Instant::now();
    let mut virtual_elapsed = mergeability_wait_is_virtual().then_some(Duration::ZERO);
    let mut attempt = 0_u32;
    loop {
        match forge.pull_request_blockers(repo_root, number) {
            Ok(Some(blockers)) => return Ok(KnownMergeBlockers::Ready(blockers)),
            Ok(None) => {
                return Ok(KnownMergeBlockers::Unavailable(format!(
                    "the forge cannot say what holds pull request #{number} back"
                )));
            }
            Err(OrbitError::PullRequestMergeabilityUnknown { .. }) => {
                let elapsed = virtual_elapsed.unwrap_or_else(|| started.elapsed());
                let remaining = MERGEABILITY_UNKNOWN_BUDGET.saturating_sub(elapsed);
                // One more step than the documented backoff, so a clock that
                // does not advance cannot spin.
                if remaining.is_zero() || attempt > MERGEABILITY_UNKNOWN_BACKOFF_SECS.len() as u32 {
                    return Ok(KnownMergeBlockers::Unavailable(
                        OrbitError::PullRequestMergeabilityUnknown { number }.to_string(),
                    ));
                }
                let delay = mergeability_backoff(attempt, remaining);
                attempt = attempt.saturating_add(1);
                if delay.is_zero() {
                    return Ok(KnownMergeBlockers::Unavailable(
                        OrbitError::PullRequestMergeabilityUnknown { number }.to_string(),
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
