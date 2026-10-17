//! Close a task's Orbit-authored PRs once nothing will deliver them.
//!
//! Orbit opens delivery PRs and `[BLOCKED]` preservation PRs, and nothing
//! else closes them, so a task that landed through another PR or a commit, or
//! that was rejected or archived, left stale PRs open. Every status writer
//! calls [`OrbitRuntime::close_task_prs_after_transition`] after its write; a
//! transition *into* done, rejected or archived closes them. Leaving done
//! never closes anything: the done transition already settled which PRs
//! close, and archiving a done task must not close the landing it
//! deliberately kept open.
//!
//! An operator or agent moving a task back to `backlog` or `someday` closes
//! them too ([`OrbitRuntime::close_task_prs_after_requeue`]), since the task's
//! next attempt opens its own pull request, except a pull request a later run
//! may still continue: everything while a run or claim of the task is live
//! (a leaf may still be landing it), everything a failure handoff kept when
//! the task leaves `blocked` and everything a final-recovery requeue
//! preserved (the next run resumes that candidate), and the pull request of
//! a candidate the owner kept from the task's last claim (its next claim
//! republishes through it). `--discard-candidate` drops those exceptions but
//! the live one. Automation's own holds and requeues close nothing: they keep
//! the candidate for the next run, whose `pr_open` closes or reuses the
//! earlier pull request. That `pr_open` closes every other open delivery PR
//! of the task once its own is open, so a re-admitted task ends with one.
//!
//! A PR qualifies only when all of these hold:
//! - its head branch is `orbit/<TASK-ID>-…`;
//! - its body names the task;
//! - it was authored by a delivery identity (`pr.delivery_authors`, else the
//!   login this machine's forge credentials act as), or it is the delivery PR
//!   of a follower handoff this owner accepted for the task.
//!
//! On done, the landing is never closed: a PR the landing note names (`#N`),
//! or, when the note names no landing and the task leaves `review`, the PR it
//! was reviewed through. The whole pass
//! is best effort: the transition has already been written, so a forge error is
//! a warning (log plus session event), never a failure. Branches are kept.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus,
    names_task_delivery_pull_request,
};
use orbit_types::workflow::handoff::HandoffDelivery;

use crate::OrbitRuntime;
use crate::runtime::task::FINAL_RECOVERY_REQUEUED_EVENT;

impl OrbitRuntime {
    /// Close `task`'s open Orbit-authored PRs when this write moved it from
    /// `previous` into done, rejected or archived. `note` is the transition's
    /// status note: the landing on done, the reason otherwise. Returns false
    /// when a forge error left the pass unfinished, so a caller that records
    /// the pass can retry it.
    pub(crate) fn close_task_prs_after_transition(
        &self,
        previous: TaskStatus,
        task: &Task,
        note: Option<&str>,
    ) -> bool {
        // A done task's landing PR was kept open on purpose, and the done
        // transition already closed the rest, so leaving done (an archive)
        // has nothing left to close.
        if previous == task.status || previous == TaskStatus::Done || !task.status.is_terminal() {
            return true;
        }
        if !self.context.settings().pr_settings().close_on_terminal {
            return true;
        }
        let landing_prs = if task.status == TaskStatus::Done {
            landing_pull_requests(previous, task, note)
        } else {
            BTreeSet::new()
        };
        self.close_open_task_prs(
            task,
            &landing_prs,
            &closure_comment(task, note),
            "closed an Orbit-authored pull request on the task's terminal decision",
        )
    }

    /// Close `task`'s open Orbit-authored PRs when an operator or agent moved
    /// it from `previous` back to `backlog` or `someday`, keeping those a
    /// later run may still continue (see the module docs). `discarded` says
    /// the same write discarded the task's preserved candidate. Returns
    /// false when a forge error left the pass unfinished.
    pub(crate) fn close_task_prs_after_requeue(
        &self,
        previous: TaskStatus,
        task: &Task,
        note: Option<&str>,
        discarded: bool,
    ) -> bool {
        if previous == task.status
            || previous == TaskStatus::Proposed
            || !matches!(task.status, TaskStatus::Backlog | TaskStatus::Someday)
            || !self.context.settings().pr_settings().close_on_terminal
        {
            return true;
        }
        if self.task_delivery_is_live(task) {
            return true;
        }
        let mut keep = BTreeSet::new();
        if !discarded {
            if previous == TaskStatus::Blocked || self.requeued_by_final_recovery(task, previous) {
                return true;
            }
            if let Some(kept) = self.kept_claim_pull_request(&task.id) {
                keep.insert(kept);
            }
        }
        self.close_open_task_prs(
            task,
            &keep,
            &requeue_comment(task, note),
            "closed an Orbit-authored pull request the task's requeue left undelivered",
        )
    }

    /// Close `task`'s open Orbit-authored PRs other than `keep`, commenting
    /// `comment` on each. Returns false when a forge error left the pass
    /// unfinished.
    fn close_open_task_prs(
        &self,
        task: &Task,
        keep: &BTreeSet<u64>,
        comment: &str,
        closed_message: &'static str,
    ) -> bool {
        let settings = self.context.settings().pr_settings();
        // Orbit records every PR it opens on the task, and every follower PR it
        // accepts on the claim. A task with neither never had an Orbit PR, so it
        // costs no forge call (local delivery, proposals, manual tasks).
        let follower_prs = self.accepted_handoff_pull_requests(&task.id);
        let recorded = task
            .external_refs
            .iter()
            .any(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM);
        if !recorded && follower_prs.is_empty() {
            return true;
        }

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let open = match forge.open_pull_requests(&repo_root) {
            Ok(open) => open,
            Err(error) => {
                self.warn_pr_closure(&task.id, None, &error);
                return false;
            }
        };
        let candidates = open
            .into_iter()
            .filter(|pr| names_task_delivery_pull_request(&pr.head_branch, &pr.body, &task.id))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return true;
        }

        let mut settled = true;
        let authors = if settings.delivery_authors.is_empty() {
            match forge.authenticated_login(&repo_root) {
                Ok(login) => vec![login.to_ascii_lowercase()],
                // Without a delivery identity only accepted follower PRs can
                // be proven Orbit's; the rest stay open.
                Err(error) => {
                    self.warn_pr_closure(&task.id, None, &error);
                    settled = false;
                    Vec::new()
                }
            }
        } else {
            settings.delivery_authors.clone()
        };

        for pr in candidates {
            let authored = authors.contains(&pr.author.to_ascii_lowercase())
                || follower_prs.contains(&pr.number);
            if !authored || keep.contains(&pr.number) {
                continue;
            }
            match forge.close_pull_request(&repo_root, pr.number, comment) {
                Ok(()) => {
                    tracing::info!(
                        target: "orbit.core.pr_closure",
                        task_id = %task.id,
                        pr = pr.number,
                        status = %task.status,
                        "{closed_message}"
                    );
                    self.record_session_event(OrbitEvent::TaskPullRequestClosed {
                        task_id: task.id.clone(),
                        pr_number: pr.number,
                    });
                }
                Err(error) => {
                    self.warn_pr_closure(&task.id, Some(pr.number), &error);
                    settled = false;
                }
            }
        }
        settled
    }

    /// Whether a run or an unsettled claim of `task` may still be delivering
    /// one of its pull requests. An unreadable answer counts as live, so
    /// nothing is closed on a guess.
    fn task_delivery_is_live(&self, task: &Task) -> bool {
        if let Some(run_id) = task.job_run_id.as_deref() {
            match self.stores().jobs().get_job_run(run_id) {
                Ok(Some(run)) if !run.state.is_terminal() => return true,
                Ok(_) => {}
                Err(_) => return true,
            }
        }
        match self.stores().tasks().inspect_execution_claims() {
            Ok(claims) => claims
                .iter()
                .any(|claim| claim.claim.task_id == task.id && claim.claim.phase.is_unsettled()),
            Err(_) => true,
        }
    }

    /// Whether `task` entered `previous` through a final-recovery requeue
    /// that no discard has since overridden, or its current run's recorded
    /// decision is a requeue that preserved its candidate (an operator or
    /// cleared-infra requeue). Its next run resumes the published candidate
    /// through the same pull request.
    fn requeued_by_final_recovery(&self, task: &Task, previous: TaskStatus) -> bool {
        let Ok(history) = self.get_task_history(&task.id) else {
            return true;
        };
        if super::requeue::current_run_requeue_stands(task, &history) {
            return true;
        }
        history
            .iter()
            .rev()
            .find(|entry| {
                entry.to_status == Some(previous) || entry.event == CANDIDATE_DISCARDED_EVENT
            })
            .is_some_and(|entry| entry.event == FINAL_RECOVERY_REQUEUED_EVENT)
    }

    /// The pull request of the candidate the owner kept from `task_id`'s last
    /// claim, when the next claim would continue it.
    fn kept_claim_pull_request(&self, task_id: &str) -> Option<u64> {
        let kept = self
            .stores()
            .tasks()
            .kept_claim_candidate(
                task_id,
                self.automation_machine_identity().unwrap_or_default(),
            )
            .ok()
            .flatten()?;
        if kept.fresh.is_some() {
            return None;
        }
        kept.candidate.pull_request.as_deref()?.parse().ok()
    }

    /// Delivery PR numbers of the follower handoffs this owner accepted for
    /// the task. A backend without claims, or a failed read, contributes none:
    /// those PRs then stay open rather than being guessed at.
    fn accepted_handoff_pull_requests(&self, task_id: &str) -> BTreeSet<u64> {
        let tasks = self.stores().tasks();
        let Ok(claims) = tasks.inspect_execution_claims() else {
            return BTreeSet::new();
        };
        claims
            .iter()
            .filter(|claim| claim.claim.task_id == task_id)
            .filter_map(|claim| {
                tasks
                    .find_accepted_handoff(&claim.claim.claim_id)
                    .ok()
                    .flatten()
            })
            .filter_map(|accepted| match accepted.handoff.candidate.delivery {
                HandoffDelivery::PullRequest { number } => Some(number),
                _ => None,
            })
            .collect()
    }

    fn warn_pr_closure(&self, task_id: &str, pr_number: Option<u64>, error: &OrbitError) {
        tracing::warn!(
            target: "orbit.core.pr_closure",
            task_id,
            pr = pr_number,
            error = %error,
            "could not close an Orbit-authored pull request after the task's terminal decision; \
             the transition stands"
        );
        self.record_session_event(OrbitEvent::TaskPullRequestCloseFailed {
            task_id: task_id.to_string(),
            pr_number,
            reason: error.to_string(),
        });
    }

    fn record_session_event(&self, event: OrbitEvent) {
        self.event_log.append(event);
    }
}

/// PRs a done transition must keep open: those its landing note names. When
/// the note names no landing (no `#N`, no full commit SHA) and the task leaves
/// `review`, the PR it was reviewed through (its newest recorded PR) is kept
/// too: an operator may approve that review before merging its PR, so it can
/// still be the landing vehicle.
fn landing_pull_requests(previous: TaskStatus, task: &Task, note: Option<&str>) -> BTreeSet<u64> {
    let note = note.unwrap_or_default();
    let mut landing = referenced_pr_numbers(note);
    if landing.is_empty()
        && !names_commit(note)
        && previous == TaskStatus::Review
        && let Some(reviewed) = task
            .external_refs
            .iter()
            .rev()
            .find(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .and_then(|reference| reference.id.parse().ok())
    {
        landing.insert(reviewed);
    }
    landing
}

/// Whether a note names a full 40- or 64-character commit SHA.
fn names_commit(note: &str) -> bool {
    note.split(|c: char| !c.is_ascii_hexdigit())
        .any(|word| matches!(word.len(), 40 | 64))
}

/// PR numbers a landing note names as `#N`.
fn referenced_pr_numbers(note: &str) -> BTreeSet<u64> {
    note.split('#')
        .skip(1)
        .filter_map(|rest| {
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            digits.parse().ok()
        })
        .collect()
}

fn closure_comment(task: &Task, note: Option<&str>) -> String {
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    let decision = match (task.status, note) {
        (TaskStatus::Done, Some(landing)) => {
            format!("Task {} landed: {landing}", task.id)
        }
        (TaskStatus::Done, None) => format!("Task {} is done.", task.id),
        (status, Some(reason)) => format!("Task {} was {status}: {reason}", task.id),
        (status, None) => format!("Task {} was {status}.", task.id),
    };
    format!(
        "{decision}\n\nOrbit closed this pull request because the task reached a terminal \
         decision. The branch is kept."
    )
}

fn requeue_comment(task: &Task, note: Option<&str>) -> String {
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    let decision = match note {
        Some(reason) => format!("Task {} went back to {}: {reason}", task.id, task.status),
        None => format!("Task {} went back to {}.", task.id, task.status),
    };
    format!(
        "{decision}\n\nOrbit closed this pull request because no run is delivering it and the \
         task's next attempt opens its own. The branch is kept."
    )
}
