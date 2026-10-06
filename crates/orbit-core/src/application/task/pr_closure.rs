//! Close a task's Orbit-authored PRs once the task reaches a terminal decision.
//!
//! Orbit opens delivery PRs and `[BLOCKED]` preservation PRs, and nothing
//! else closes them, so a task that landed through another PR or a commit, or
//! that was rejected or archived, left stale PRs open. Every status writer
//! calls [`OrbitRuntime::close_task_prs_after_transition`] after its write.
//! Only a transition *into* done, rejected or archived closes anything: a new
//! run, a block or a requeue never does, because a re-run resumes from the
//! `[BLOCKED]` candidate. Leaving done never closes anything either: the done
//! transition already settled which PRs close, and archiving a done task must
//! not close the landing it deliberately kept open.
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
use orbit_types::task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus};
use orbit_types::workflow::handoff::HandoffDelivery;

use crate::OrbitRuntime;
use crate::runtime::task_pr_forge::ForgePullRequest;

impl OrbitRuntime {
    /// Close `task`'s open Orbit-authored PRs when this write moved it from
    /// `previous` into done, rejected or archived. `note` is the transition's
    /// status note: the landing on done, the reason otherwise.
    pub(crate) fn close_task_prs_after_transition(
        &self,
        previous: TaskStatus,
        task: &Task,
        note: Option<&str>,
    ) {
        // A done task's landing PR was kept open on purpose, and the done
        // transition already closed the rest, so leaving done (an archive)
        // has nothing left to close.
        if previous == task.status
            || previous == TaskStatus::Done
            || !is_terminal_decision(task.status)
        {
            return;
        }
        let settings = self.context.settings().pr_settings();
        if !settings.close_on_terminal {
            return;
        }
        // Orbit records every PR it opens on the task, and every follower PR it
        // accepts on the claim. A task with neither never had an Orbit PR, so it
        // costs no forge call (local delivery, proposals, manual tasks).
        let follower_prs = self.accepted_handoff_pull_requests(&task.id);
        let recorded = task
            .external_refs
            .iter()
            .any(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM);
        if !recorded && follower_prs.is_empty() {
            return;
        }

        let repo_root = self.context.paths().repo_root.clone();
        let forge = self.task_pr_forge();
        let open = match forge.open_pull_requests(&repo_root) {
            Ok(open) => open,
            Err(error) => {
                self.warn_pr_closure(&task.id, None, &error);
                return;
            }
        };
        let candidates = open
            .into_iter()
            .filter(|pr| names_task(pr, &task.id))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }

        let authors = if settings.delivery_authors.is_empty() {
            match forge.authenticated_login(&repo_root) {
                Ok(login) => vec![login.to_ascii_lowercase()],
                // Without a delivery identity only accepted follower PRs can
                // be proven Orbit's; the rest stay open.
                Err(error) => {
                    self.warn_pr_closure(&task.id, None, &error);
                    Vec::new()
                }
            }
        } else {
            settings.delivery_authors.clone()
        };
        let landing_prs = if task.status == TaskStatus::Done {
            landing_pull_requests(previous, task, note)
        } else {
            BTreeSet::new()
        };
        let comment = closure_comment(task, note);

        for pr in candidates {
            let authored = authors.contains(&pr.author.to_ascii_lowercase())
                || follower_prs.contains(&pr.number);
            if !authored || landing_prs.contains(&pr.number) {
                continue;
            }
            match forge.close_pull_request(&repo_root, pr.number, &comment) {
                Ok(()) => {
                    tracing::info!(
                        target: "orbit.core.pr_closure",
                        task_id = %task.id,
                        pr = pr.number,
                        status = %task.status,
                        "closed an Orbit-authored pull request on the task's terminal decision"
                    );
                    self.record_session_event(OrbitEvent::TaskPullRequestClosed {
                        task_id: task.id.clone(),
                        pr_number: pr.number,
                    });
                }
                Err(error) => self.warn_pr_closure(&task.id, Some(pr.number), &error),
            }
        }
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

fn is_terminal_decision(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
    )
}

/// The branch and body tests: `orbit/<TASK-ID>-…`, and a body naming the
/// task as a whole token, so `ORB-1` never matches `ORB-10`'s PRs.
fn names_task(pr: &ForgePullRequest, task_id: &str) -> bool {
    let branch_prefix = format!("orbit/{task_id}-");
    pr.head_branch.starts_with(&branch_prefix) && contains_token(&pr.body, task_id)
}

fn contains_token(text: &str, token: &str) -> bool {
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(token).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + token.len()..].chars().next();
        !before.is_some_and(is_id_char) && !after.is_some_and(is_id_char)
    })
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
