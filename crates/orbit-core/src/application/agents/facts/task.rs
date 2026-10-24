use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_types::task::{
    GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskComment, TaskHistoryEntry, TaskRelationType,
    TaskStatus,
};
use orbit_types::workflow::ReviewVerdict;

use super::super::records::{
    Cause, blocked_pr, bounded, cites_criteria, is_operator_requeue, landed, landing_pr,
    restart_cause, run_spans, spec_blocker_kind, spec_documents,
};
use super::super::roles::pilot_warnings::settle;
use super::attempts::{assigned_runner, runner};
use super::model::{Attempt, FiledFinding, Problem, Record, Review, RunRecord, Runner, TaskFacts};
use super::review::{failed_fix, settlement};
use crate::application::task::{is_review_finding, latest_verification};

/// The facts of one task. `classify` gives the cause a block or run exit
/// records.
pub(super) fn task_facts(
    task: &Task,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    runs: &[RunRecord],
    pulled: &HashMap<&str, Runner>,
    classify: &dyn Fn(&TaskHistoryEntry) -> Cause,
    now: DateTime<Utc>,
) -> TaskFacts {
    let spans = run_spans(history);
    let attempts: Vec<Attempt> = spans
        .iter()
        .map(|span| Attempt {
            start: span.start,
            end: span.end,
            runner: runner(runs, span.start, span.end.or(Some(now)))
                .unwrap_or_else(|| assigned_runner(task, history, pulled, span.start)),
        })
        .collect();
    let landed = landed(task.status, history);
    let dropped = matches!(task.status, TaskStatus::Archived | TaskStatus::Rejected) && !landed;
    let landed_at = landed
        .then(|| {
            history
                .iter()
                .rev()
                .find(|entry| entry.to_status == Some(TaskStatus::Done))
                .map(|entry| entry.at)
        })
        .flatten();
    let final_attempt = landed
        .then(|| match landed_at {
            Some(at) => attempts.iter().rposition(|attempt| attempt.start <= at),
            None => attempts.len().checked_sub(1),
        })
        .flatten();
    let mut records = Vec::new();

    // Spec defects recorded on the task's own history. An edited task counts
    // once, however many edits it had.
    let first_start = attempts.first().map(|attempt| attempt.start);
    let mut edits = history.iter().filter_map(|entry| {
        let documents = spec_documents(entry)?;
        first_start
            .is_some_and(|first| entry.at > first)
            .then_some((entry, documents))
    });
    if let Some((entry, documents)) = edits.next() {
        let later = edits.count();
        records.push(Record {
            problem: Problem::SpecEdit,
            at: entry.at,
            detail: if later == 0 {
                format!("{} edited by {}", documents.join(" and "), entry.by)
            } else {
                format!(
                    "{} edited by {}, then {later} more edit(s)",
                    documents.join(" and "),
                    entry.by
                )
            },
            cause: None,
            ended: None,
        });
    }
    for entry in history {
        if let Some(kind) = spec_blocker_kind(entry) {
            records.push(Record {
                problem: Problem::CriteriaBlock,
                at: entry.at,
                detail: format!("agent blocker kind {kind}"),
                cause: None,
                ended: None,
            });
        }
        if cites_criteria(entry) {
            records.push(Record {
                problem: Problem::CriteriaEscalation,
                at: entry.at,
                detail: bounded(entry.note.as_deref().unwrap_or_default()),
                cause: None,
                ended: None,
            });
        }
    }

    // `[BLOCKED]` PRs, then delivery PRs that a settled task never merged.
    let mut blocked_prs = BTreeSet::new();
    for entry in history {
        let Some(number) = blocked_pr(entry) else {
            continue;
        };
        if blocked_prs.insert(number.to_string()) {
            records.push(Record {
                problem: Problem::BlockedPr,
                at: entry.at,
                detail: format!("[BLOCKED] PR #{number}"),
                cause: Some(classify(entry)),
                ended: None,
            });
        }
    }
    let landing = landed.then(|| landing_pr(history)).flatten();
    if dropped || landing.is_some() {
        let settled_at = history
            .iter()
            .rev()
            .find(|entry| entry.to_status.is_some_and(TaskStatus::is_terminal))
            .map_or(task.updated_at, |entry| entry.at);
        for reference in &task.external_refs {
            if reference.system != GITHUB_PR_EXTERNAL_REF_SYSTEM
                || blocked_prs.contains(&reference.id)
                || landing.as_deref() == Some(reference.id.as_str())
            {
                continue;
            }
            records.push(Record {
                problem: Problem::UnmergedPr,
                at: settled_at,
                detail: format!("PR #{} closed without merging", reference.id),
                cause: Some(Cause::Other),
                ended: None,
            });
        }
    }

    // Each run after the first, attributed to what ended the one before it.
    for (index, pair) in attempts.windows(2).enumerate() {
        let [previous, attempt] = pair else { continue };
        records.push(Record {
            problem: Problem::RepeatRun,
            at: attempt.start,
            detail: format!("run {} of {}", index + 2, attempts.len()),
            cause: Some(restart_cause(
                history,
                Some(previous.start),
                attempt.start,
                classify,
            )),
            ended: Some(index),
        });
    }

    // Blocked tasks returned to the backlog by hand.
    for entry in history {
        if !is_operator_requeue(entry) {
            continue;
        }
        let ended = attempts
            .iter()
            .rposition(|attempt| attempt.start < entry.at);
        records.push(Record {
            problem: Problem::OperatorRequeue,
            at: entry.at,
            detail: format!("{} by {}", entry.event, entry.by),
            cause: Some(restart_cause(
                history,
                ended.map(|index| attempts[index].start),
                entry.at,
                classify,
            )),
            ended,
        });
    }

    // Review gate settlements and escalations of reviewer fixes, from the
    // comments the gate posts. A replayed settlement is counted once.
    let mut reviews: Vec<Review> = Vec::new();
    for comment in comments {
        if let Some(review) = settlement(comment) {
            if !reviews
                .iter()
                .any(|seen| seen.attempt_id == review.attempt_id)
            {
                reviews.push(review);
            }
        } else if failed_fix(comment) {
            if let Some(review) = reviews.iter_mut().rfind(|review| review.at <= comment.at) {
                review.fix_failed = true;
            }
            records.push(Record {
                problem: Problem::FailedFix,
                at: comment.at,
                detail: "reviewer fixes failed owner revalidation".to_string(),
                cause: None,
                ended: None,
            });
        }
    }
    // An incomplete review is infrastructure's when the block that followed
    // its settlement is.
    for review in &mut reviews {
        if review.verdict == ReviewVerdict::Incomplete {
            review.infrastructure = history
                .iter()
                .filter(|entry| {
                    entry.at >= review.at
                        && entry
                            .to_status
                            .is_some_and(|status| status != TaskStatus::InProgress)
                })
                .min_by_key(|entry| entry.at)
                .is_some_and(|entry| classify(entry) == Cause::Infrastructure);
        }
    }
    let filed = is_review_finding(task).then(|| FiledFinding {
        filer: task
            .relations
            .iter()
            .find(|relation| relation.relation_type == TaskRelationType::SpawnedFrom)
            .map(|relation| relation.target.to_string()),
        verdict: latest_verification(history)
            .map(|record| (record.verdict, record.verified_at, record.severity)),
    });

    records.sort_by_key(|record| record.at);
    let warnings = settle(task, history, comments);
    TaskFacts {
        id: task.id.to_string(),
        title: task.title.clone(),
        status: task.status,
        orchestrator: task
            .orchestrator
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        orchestrator_model: task
            .orchestrator_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string),
        origin: task.origin,
        tags: task.tags.clone(),
        context_files: task.context_files.clone(),
        complexity: task.complexity,
        created_at: task.created_at,
        landed,
        dropped,
        attempts,
        final_attempt,
        landed_at,
        records,
        reviews,
        filed,
        warnings,
    }
}
