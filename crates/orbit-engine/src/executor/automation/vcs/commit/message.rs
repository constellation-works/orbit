//! The one composer of every commit and squash message Orbit writes. Each
//! message follows the grammar in [`orbit_types::workflow::CommitMessage`]:
//! `<type>: <summary> [ID]… (#PR)`, a prose body, and one final trailer
//! block in registry order.

use std::collections::BTreeMap;

use orbit_types::task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskRelationType};
use orbit_types::workflow::{
    CommitMessage, CommitRole, CommitType, Trailer, TrailerKey, collect_trailers, compose_subject,
    orbit_identity, orbit_role_for_email, validate_subject,
};

use crate::context::PrConfig;

use super::super::attribution::orchestration_trailer;
use super::super::conventions::{TaskValues, meaningful_execution_summary};
use super::author::GitAuthor;

/// The summary of the commit that keeps a stopped reviewer's edits; its
/// subject is `<type>: partial reviewer repairs preserved [ID]`.
const PARTIAL_REPAIR_SUMMARY: &str = "partial reviewer repairs preserved";

/// A `regression_from` target's landed commit, resolved by the caller from
/// the target's recorded delivery or landing evidence. `sha` is the full
/// commit id; composition abbreviates it. Absent from the map when no landed
/// commit could be resolved, which omits only the `Fixes:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegressionLandedCommit {
    pub sha: String,
    pub subject: String,
}

/// Landed commits by `regression_from` target task id.
pub(super) type RegressionLandedCommits = BTreeMap<String, RegressionLandedCommit>;

/// One commit of a pull request's `base..head`, read for its squash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InnerCommit {
    pub author_name: String,
    pub author_email: String,
    pub message: String,
}

/// The commit type of the work `tasks` deliver.
fn commit_type_for(tasks: &[Task]) -> CommitType {
    CommitType::for_bundle(tasks.iter().map(|task| task.task_type))
}

/// The message of a commit delivering `tasks`: one task's work, or a bundle
/// (finalize) committing several. `co_authors` become `Co-authored-by`.
pub(super) fn delivery_commit_message(
    tasks: &[Task],
    pr_config: &PrConfig,
    landed: &RegressionLandedCommits,
    co_authors: &[GitAuthor],
) -> String {
    let mut message = delivery_parts(tasks, pr_config, None);
    message
        .trailers
        .extend(task_trailers(tasks, pr_config, landed));
    message
        .trailers
        .extend(co_authors.iter().map(GitAuthor::co_author_trailer));
    message.render()
}

/// The squash message of pull request `pr_number` delivering `tasks`:
/// subject and body from task state, trailers collected from `inner`
/// commits (a recovery or review commit contributes only its trailers), and
/// one `Co-authored-by` per distinct inner author. Identities Orbit wrote
/// before the one scheme are rewritten to it.
pub(super) fn squash_merge_message(
    tasks: &[Task],
    pr_config: &PrConfig,
    pr_number: u64,
    inner: &[InnerCommit],
) -> CommitMessage {
    let mut message = if tasks.is_empty() {
        inner_commit_parts(inner, pr_number)
    } else {
        delivery_parts(tasks, pr_config, Some(pr_number))
    };
    message.trailers.extend(
        tasks
            .iter()
            .map(|task| Trailer::new(TrailerKey::OrbitTask, &task.id)),
    );
    if inner.is_empty() {
        // Nothing to collect from: fall back to what task state knows.
        message.trailers.extend(task_trailers(
            tasks,
            pr_config,
            &RegressionLandedCommits::new(),
        ));
    }
    for commit in inner {
        for trailer in collect_trailers(&commit.message) {
            let trailer = if trailer.key == TrailerKey::CoAuthoredBy {
                Trailer::new(
                    TrailerKey::CoAuthoredBy,
                    current_identity_spec(&trailer.value),
                )
            } else {
                trailer
            };
            message.trailers.push(trailer);
        }
        message.trailers.push(Trailer::new(
            TrailerKey::CoAuthoredBy,
            current_identity(&commit.author_name, &commit.author_email),
        ));
    }
    message
}

/// Subject and body when the delivered tasks cannot be read: the oldest
/// inner commit with a standard subject was composed from task state, so its
/// type, summary, ids and prose carry over with ` (#PR)` added.
fn inner_commit_parts(inner: &[InnerCommit], pr_number: u64) -> CommitMessage {
    for commit in inner {
        let mut lines = commit.message.lines();
        let subject = lines.next().unwrap_or_default().trim();
        if validate_subject(subject).is_err() {
            continue;
        }
        let Some((kind, rest)) = subject.split_once(": ") else {
            continue;
        };
        let Some(kind) = CommitType::parse(kind) else {
            continue;
        };
        let mut summary = rest.trim();
        let mut ids = Vec::new();
        while let Some(open) = summary.rfind(" [")
            && let Some(id) = summary[open + 2..].strip_suffix(']')
            && !id.contains([' ', '['])
        {
            ids.push(id);
            summary = summary[..open].trim_end();
        }
        ids.reverse();
        let composed = compose_subject(kind, summary, &ids, Some(pr_number));
        let body = commit
            .message
            .split("\n\n")
            .skip(1)
            .map(str::trim)
            .filter(|paragraph| {
                !paragraph.is_empty() && collect_trailers(&format!("x\n\n{paragraph}")).is_empty()
            })
            .map(str::to_string)
            .collect();
        return CommitMessage {
            subject: composed.subject,
            body,
            trailers: Vec::new(),
        };
    }
    CommitMessage {
        subject: compose_subject(
            CommitType::Chore,
            "deliver pull request",
            &[],
            Some(pr_number),
        )
        .subject,
        body: Vec::new(),
        trailers: Vec::new(),
    }
}

/// A step recovery's repair commit of `task_ids`.
pub(super) fn recovery_commit_message(
    commit_type: CommitType,
    task_ids: &[String],
    run_id: &str,
    failed_step_id: &str,
    recovery_activity: &str,
) -> String {
    let ids = task_ids.iter().map(String::as_str).collect::<Vec<_>>();
    let subject = compose_subject(
        commit_type,
        &format!("repair failed `{failed_step_id}` step from recovery"),
        &ids,
        None,
    );
    let mut trailers = task_id_trailers(task_ids);
    trailers.extend([
        Trailer::new(TrailerKey::OrbitRecoveryRun, run_id),
        Trailer::new(TrailerKey::OrbitRecoveryStep, failed_step_id),
        Trailer::new(TrailerKey::OrbitRecoveryActivity, recovery_activity),
    ]);
    CommitMessage {
        subject: subject.subject,
        body: vec![format!(
            "Recovery repaired the working tree with Git metadata read-only; Orbit \
             committed the repair and retries `{failed_step_id}` on this commit."
        )],
        trailers,
    }
    .render()
}

/// The commit keeping a stopped reviewer's uncommitted edits of `task_id`.
pub(in crate::executor::automation::vcs) fn partial_repair_message(
    commit_type: CommitType,
    task_id: &str,
    run_id: &str,
    review_step_id: &str,
    attempt_id: Option<&str>,
) -> String {
    let subject = compose_subject(commit_type, PARTIAL_REPAIR_SUMMARY, &[task_id], None);
    let mut trailers = vec![Trailer::new(TrailerKey::OrbitTask, task_id)];
    if let Some(attempt_id) = attempt_id.map(str::trim).filter(|id| !id.is_empty()) {
        trailers.push(Trailer::new(TrailerKey::OrbitReviewAttempt, attempt_id));
    }
    trailers.extend([
        Trailer::new(TrailerKey::OrbitReviewRun, run_id),
        Trailer::new(TrailerKey::OrbitReviewStep, review_step_id),
    ]);
    CommitMessage {
        subject: subject.subject,
        body: Vec::new(),
        trailers,
    }
    .render()
}

/// Whether `message` is a partial-repair commit, of `run_id` when given.
/// Accepts the historical `review: …` subject and `Orbit-Review-Run:` line.
pub(in crate::executor::automation::vcs) fn is_partial_repair_message(
    message: &str,
    run_id: Option<&str>,
) -> bool {
    let subject_matches = message
        .lines()
        .next()
        .is_some_and(|subject| subject.contains(PARTIAL_REPAIR_SUMMARY));
    let run_key = TrailerKey::OrbitReviewRun.as_str();
    subject_matches
        && message.lines().any(|line| {
            let Some((key, value)) = line.trim().split_once(':') else {
                return false;
            };
            key == run_key && run_id.is_none_or(|run_id| value.trim() == run_id)
        })
}

/// The reviewer's repair commit of `tasks`: `summary` (the review's first
/// line) as the subject, the repaired findings and changed paths as prose,
/// and the attempt and crew as trailers.
pub fn reviewer_repair_message(
    tasks: &[Task],
    summary: &str,
    finding_ids: &[String],
    paths: &[String],
    attempt_id: &str,
    crew: &str,
) -> String {
    let summary = summary.trim().lines().next().unwrap_or_default().trim();
    let summary = if summary.is_empty() {
        "reviewer repairs"
    } else {
        summary
    };
    let ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let subject = compose_subject(commit_type_for(tasks), summary, &ids, None);
    let mut body = Vec::new();
    if subject.summary_cut {
        body.push(summary.to_string());
    }
    body.push(format!(
        "Repaired findings: {}.",
        if finding_ids.is_empty() {
            "none named".to_string()
        } else {
            finding_ids.join(", ")
        }
    ));
    body.push(format!("Changed paths: {}.", paths.join(", ")));
    let mut trailers = tasks
        .iter()
        .map(|task| Trailer::new(TrailerKey::OrbitTask, &task.id))
        .collect::<Vec<_>>();
    trailers.extend([
        Trailer::new(TrailerKey::OrbitReviewAttempt, attempt_id),
        Trailer::new(TrailerKey::OrbitReviewCrew, crew),
    ]);
    CommitMessage {
        subject: subject.subject,
        body,
        trailers,
    }
    .render()
}

/// Subject and body for `tasks`, with ` (#PR)` when `pr_number` is known.
fn delivery_parts(tasks: &[Task], pr_config: &PrConfig, pr_number: Option<u64>) -> CommitMessage {
    match tasks {
        [task] => single_task_parts(task, pr_config, pr_number),
        _ => bundle_parts(tasks, pr_number),
    }
}

/// `commit.subject_template` and `commit.body_template` shape a single task's
/// subject and body. A templated subject that fails the grammar falls back to
/// the standard subject, so every subject Orbit writes validates.
fn single_task_parts(task: &Task, pr_config: &PrConfig, pr_number: Option<u64>) -> CommitMessage {
    let conventions = &pr_config.conventions;
    let values = TaskValues {
        task,
        pr_config,
        freshness: None,
    };
    let title = task.title.trim();
    let templated = conventions.commit_subject.as_ref().and_then(|template| {
        let (mut subject, shortened) = values.render_subject(template, pr_number);
        if let Some(number) = pr_number {
            subject.push_str(&format!(" (#{number})"));
        }
        match validate_subject(&subject) {
            Ok(()) => Some((subject, shortened)),
            Err(violation) => {
                tracing::warn!(
                    task_id = %task.id,
                    %violation,
                    "commit.subject_template rendered a subject outside the commit standard; \
                     using the standard subject"
                );
                None
            }
        }
    });
    let (subject, cut) = templated.unwrap_or_else(|| {
        let markers = subject_markers(task);
        let markers = markers.iter().map(String::as_str).collect::<Vec<_>>();
        let composed = compose_subject(
            CommitType::for_task_type(task.task_type),
            title,
            &markers,
            pr_number,
        );
        (composed.subject, composed.summary_cut)
    });
    let mut body = Vec::new();
    if cut && !title.is_empty() {
        body.push(title.to_string());
    }
    match conventions.commit_body.as_ref() {
        Some(template) => body.push(values.render(template).trim().to_string()),
        None => body.extend(execution_summary_paragraph(task)),
    }
    CommitMessage {
        subject,
        body,
        trailers: Vec::new(),
    }
}

/// A bundle names every task id in its own brackets and lists one
/// `- ID: summary` line per task.
fn bundle_parts(tasks: &[Task], pr_number: Option<u64>) -> CommitMessage {
    let ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let titles = tasks
        .iter()
        .map(|task| task.title.trim())
        .filter(|title| !title.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let subject = compose_subject(commit_type_for(tasks), &titles, &ids, pr_number);
    let lines = tasks
        .iter()
        .map(|task| {
            let summary =
                execution_summary_paragraph(task).unwrap_or_else(|| single_line(task.title.trim()));
            format!("- {}: {summary}", task.id)
        })
        .collect::<Vec<_>>()
        .join("\n");
    CommitMessage {
        subject: subject.subject,
        body: vec![lines],
        trailers: Vec::new(),
    }
}

/// The task's own marker, then its non-Orbit ticket refs (`[JIRA-123]`).
/// Orbit's own `github-pr` refs are delivery bookkeeping, not a ticket: a
/// failure-handoff PR recorded there may be closed unmerged, so naming it
/// would point history at a PR that never delivered the commit.
fn subject_markers(task: &Task) -> Vec<String> {
    let mut markers = vec![task.id.clone()];
    markers.extend(
        task.external_refs
            .iter()
            .filter(|external_ref| external_ref.system != GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .map(|external_ref| {
                format!(
                    "{}-{}",
                    external_ref.system.to_ascii_uppercase(),
                    external_ref.id
                )
            }),
    );
    markers
}

/// `Orbit-Task` per task, regression groups and `Orchestrated-By`.
fn task_trailers(
    tasks: &[Task],
    pr_config: &PrConfig,
    landed: &RegressionLandedCommits,
) -> Vec<Trailer> {
    let mut trailers = tasks
        .iter()
        .map(|task| Trailer::new(TrailerKey::OrbitTask, &task.id))
        .collect::<Vec<_>>();
    trailers.extend(regression_trailers(tasks, pr_config, landed));
    if pr_config.conventions.orchestrated_by_trailer
        && let Some(value) = orchestration_trailer(tasks)
    {
        trailers.push(Trailer::new(TrailerKey::OrchestratedBy, value));
    }
    trailers
}

fn task_id_trailers(task_ids: &[String]) -> Vec<Trailer> {
    task_ids
        .iter()
        .map(|id| Trailer::new(TrailerKey::OrbitTask, id))
        .collect()
}

/// One `Fixes` / `Regression-From` / `Regression` group per
/// `regression_from` relation, in relation order, across `tasks` in order.
/// Rendering keeps keys in registry order, so the groups interleave by key.
fn regression_trailers(
    tasks: &[Task],
    pr_config: &PrConfig,
    landed: &RegressionLandedCommits,
) -> Vec<Trailer> {
    if !pr_config.conventions.regression_trailers {
        return Vec::new();
    }
    let mut trailers = Vec::new();
    for task in tasks {
        for relation in &task.relations {
            if relation.relation_type != TaskRelationType::RegressionFrom {
                continue;
            }
            if let Some(commit) = landed.get(relation.target.as_str()) {
                let sha: String = commit.sha.trim().chars().take(12).collect();
                let subject = commit.subject.trim();
                if !sha.is_empty() && !subject.is_empty() {
                    trailers.push(Trailer::new(
                        TrailerKey::Fixes,
                        format!("{sha} (\"{subject}\")"),
                    ));
                }
            }
            trailers.push(Trailer::new(TrailerKey::RegressionFrom, &relation.target));
            if let Some(regression) = &relation.regression {
                trailers.push(Trailer::new(
                    TrailerKey::Regression,
                    format!(
                        "{}; escape={}; severity={}",
                        regression.nature.as_str(),
                        regression.escape.as_str(),
                        regression.severity.as_str()
                    ),
                ));
            }
        }
    }
    trailers
}

/// `name <email>` in the current scheme when `email` is an address Orbit
/// wrote (old or new); anyone else's identity is kept as written.
fn current_identity(name: &str, email: &str) -> String {
    match orbit_role_for_email(email) {
        Some(role) => {
            let model = name
                .trim()
                .strip_prefix("orbit[")
                .and_then(|rest| rest.strip_suffix(']'));
            let (name, email) = orbit_identity(role, model);
            format!("{name} <{email}>")
        }
        None => format!("{} <{}>", name.trim(), email.trim()),
    }
}

/// [`current_identity`] for a `name <email>` trailer value.
fn current_identity_spec(spec: &str) -> String {
    match spec.trim().rsplit_once('<') {
        Some((name, email)) => current_identity(name, email.trim_end_matches('>')),
        None => spec.trim().to_string(),
    }
}

/// The identity a role commits under, as `GitAuthor`.
pub(super) fn role_author(role: CommitRole, model: Option<&str>) -> GitAuthor {
    let (name, email) = orbit_identity(role, model);
    GitAuthor::new(name, email)
}

/// The body's summary paragraph: the first paragraph of the execution
/// summary's `## Summary` (or `## 1. Summary of Changes`) section, else the
/// first prose paragraph of the whole summary, flattened to one line.
fn execution_summary_paragraph(task: &Task) -> Option<String> {
    let summary = meaningful_execution_summary(&task.execution_summary)?;
    let section = extract_summary_section(summary).unwrap_or_else(|| summary.to_string());
    let paragraph = section
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty() || line.starts_with('#'))
        .take_while(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.trim_start_matches("- ")
                .trim_start_matches("* ")
                .trim()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let paragraph = paragraph.trim();
    (!paragraph.is_empty()).then(|| single_line(paragraph))
}

fn extract_summary_section(summary: &str) -> Option<String> {
    let mut in_section = false;
    let mut lines = Vec::new();

    for line in summary.lines() {
        let trimmed = line.trim();
        let is_heading = trimmed.starts_with("## ");
        if trimmed == "## 1. Summary of Changes" || trimmed == "## Summary" {
            in_section = true;
            continue;
        }
        if in_section && is_heading {
            break;
        }
        if in_section {
            lines.push(trimmed.to_string());
        }
    }

    let section = lines.join("\n");
    let section = section.trim();
    (!section.is_empty()).then_some(section.to_string())
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
