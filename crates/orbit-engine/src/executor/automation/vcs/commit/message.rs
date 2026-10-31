use orbit_types::task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task};
use orbit_types::workflow::{COMMIT_SUBJECT_BUDGET, SUBJECT_ELLIPSIS};

use crate::context::PrConfig;

use super::super::attribution::orchestration_trailer;
use super::super::conventions::{TaskValues, conventional_commit_type};

/// Join a subject, body sections and a trailer block, skipping empty parts.
fn compose(subject: String, sections: Vec<String>, trailers: Vec<String>) -> String {
    let mut message = subject;
    for section in sections
        .into_iter()
        .chain((!trailers.is_empty()).then(|| trailers.join("\n")))
        .filter(|section| !section.trim().is_empty())
    {
        message.push_str("\n\n");
        message.push_str(&section);
    }
    message
}

/// The subject and body sections `commit.subject_template` and
/// `commit.body_template` give a single-task message, each falling back to
/// the form's own (`default_subject`, `default_body`).
fn templated_parts(
    task: &Task,
    pr_config: &PrConfig,
    default_subject: impl FnOnce() -> (String, Vec<String>),
    default_body: impl FnOnce() -> Vec<String>,
) -> (String, Vec<String>) {
    let conventions = &pr_config.conventions;
    let values = TaskValues {
        task,
        pr_config,
        freshness: None,
    };
    let (subject, mut sections) = match conventions.commit_subject.as_ref() {
        Some(template) => {
            let (subject, shortened) = values.render_subject(template);
            let lead = shortened.then(|| task.title.trim().to_string());
            (subject, lead.into_iter().collect())
        }
        None => default_subject(),
    };
    match conventions.commit_body.as_ref() {
        Some(template) => sections.push(values.render(template).trim().to_string()),
        None => sections.extend(default_body()),
    }
    (subject, sections)
}

fn orchestration_trailers(tasks: &[Task], pr_config: &PrConfig) -> Vec<String> {
    orchestration_trailer(tasks)
        .filter(|_| pr_config.conventions.orchestrated_by_trailer)
        .into_iter()
        .collect()
}

pub(super) fn task_commit_message(task: &Task, pr_config: &PrConfig) -> String {
    let (subject, sections) = templated_parts(
        task,
        pr_config,
        || (format!("[{}] {}", task.id, task.title.trim()), Vec::new()),
        || execution_summary_paragraph(task).into_iter().collect(),
    );
    compose(
        subject,
        sections,
        orchestration_trailers(std::slice::from_ref(task), pr_config),
    )
}

pub(super) fn finalize_commit_message(tasks: &[Task], pr_config: &PrConfig) -> String {
    if let [task] = tasks {
        let summary =
            execution_summary_paragraph(task).unwrap_or_else(|| task.title.trim().to_string());
        let line = single_line_summary(&summary);
        let (subject, sections) = templated_parts(
            task,
            pr_config,
            || (format!("fix: {line} [{}]", task.id), Vec::new()),
            || {
                (summary != line)
                    .then(|| summary.clone())
                    .into_iter()
                    .collect()
            },
        );
        return compose(subject, sections, orchestration_trailers(tasks, pr_config));
    }

    let ids_joined = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let summaries = tasks
        .iter()
        .map(|task| {
            let summary =
                execution_summary_paragraph(task).unwrap_or_else(|| task.title.trim().to_string());
            format!("- {}: {}", task.id, single_line_summary(&summary))
        })
        .collect::<Vec<_>>()
        .join("\n");

    compose(
        format!("fix: finalize ship batch [{ids_joined}]"),
        vec![summaries],
        orchestration_trailers(tasks, pr_config),
    )
}

pub(super) fn batch_commit_message(task: &Task, pr_config: &PrConfig) -> String {
    let (subject, sections) = templated_parts(
        task,
        pr_config,
        || batch_default_subject(task),
        || execution_summary_paragraph(task).into_iter().collect(),
    );
    compose(subject, sections, batch_commit_trailers(task, pr_config))
}

/// `<type>: <title> [<id>] [<SYSTEM>-<ref>]…`, with the title shortened to
/// the subject budget and then kept whole as the body's first section.
fn batch_default_subject(task: &Task) -> (String, Vec<String>) {
    let commit_type = conventional_commit_type(task.task_type);
    let title = task.title.trim();
    let (subject_title, truncated) = truncate_title_for_subject(commit_type, title);

    let mut subject = format!("{commit_type}: {subject_title} [{}]", task.id);
    // Orbit's own `github-pr` refs are delivery bookkeeping, not a ticket: a
    // failure-handoff PR recorded there may be closed unmerged, so naming it
    // would point history at a PR that never delivered the commit.
    for external_ref in task
        .external_refs
        .iter()
        .filter(|external_ref| external_ref.system != GITHUB_PR_EXTERNAL_REF_SYSTEM)
    {
        subject.push(' ');
        subject.push_str(&format!(
            "[{}-{}]",
            external_ref.system.to_ascii_uppercase(),
            external_ref.id
        ));
    }
    (
        subject,
        truncated.then(|| title.to_string()).into_iter().collect(),
    )
}

fn truncate_title_for_subject(commit_type: &str, title: &str) -> (String, bool) {
    let prefix_len = commit_type.chars().count() + ": ".chars().count();
    let title_budget = COMMIT_SUBJECT_BUDGET.saturating_sub(prefix_len);
    if title.chars().count() <= title_budget {
        return (title.to_string(), false);
    }

    let retained_chars = title_budget.saturating_sub(1);
    let mut truncated = title.chars().take(retained_chars).collect::<String>();
    truncated.push(SUBJECT_ELLIPSIS);
    (truncated, true)
}

fn batch_commit_trailers(task: &Task, pr_config: &PrConfig) -> Vec<String> {
    let conventions = &pr_config.conventions;
    let mut trailers = Vec::new();
    if let Some(planned_by) = task
        .planned_by
        .as_deref()
        .filter(|_| conventions.planned_by_trailer)
    {
        trailers.push(format!("Planned-By: {planned_by}"));
    }
    if let Some(implemented_by) = task
        .implemented_by
        .as_deref()
        .filter(|_| conventions.implemented_by_trailer)
    {
        trailers.push(format!("Implemented-By: {implemented_by}"));
    }
    trailers.extend(orchestration_trailers(
        std::slice::from_ref(task),
        pr_config,
    ));
    trailers
}

fn execution_summary_paragraph(task: &Task) -> Option<String> {
    let section = extract_summary_section(&task.execution_summary)?;
    let paragraph = section
        .lines()
        .map(str::trim)
        .map(|line| {
            line.trim_start_matches("- ")
                .trim_start_matches("* ")
                .trim()
        })
        .skip_while(|line| line.is_empty())
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let paragraph = paragraph.trim();
    (!paragraph.is_empty()).then_some(paragraph.to_string())
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

fn single_line_summary(summary: &str) -> String {
    summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}
