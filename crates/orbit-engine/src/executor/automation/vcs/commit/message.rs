use orbit_types::task::{Task, TaskType};

use super::super::attribution::orchestration_trailer;

const BATCH_SUBJECT_BUDGET: usize = 72;
const ELLIPSIS: char = '…';

pub(super) fn task_commit_message(task: &Task) -> String {
    let mut message = format!("[{}] {}", task.id, task.title.trim());
    if let Some(summary) = execution_summary_paragraph(task) {
        message.push_str("\n\n");
        message.push_str(&summary);
    }
    append_orchestration_trailers(&mut message, std::slice::from_ref(task));
    message
}

pub(super) fn finalize_commit_message(tasks: &[Task]) -> String {
    if tasks.len() == 1 {
        let task = &tasks[0];
        let summary =
            execution_summary_paragraph(task).unwrap_or_else(|| task.title.trim().to_string());
        let subject = single_line_summary(&summary);
        let mut message = format!("fix: {} [{}]", subject, task.id);
        if summary != subject {
            message.push_str("\n\n");
            message.push_str(&summary);
        }
        append_orchestration_trailers(&mut message, tasks);
        return message;
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

    let mut message = format!("fix: finalize ship batch [{ids_joined}]\n\n{summaries}");
    append_orchestration_trailers(&mut message, tasks);
    message
}

pub(super) fn batch_commit_message(task: &Task) -> String {
    let commit_type = conventional_commit_type(task.task_type);
    let title = task.title.trim();
    let (subject_title, truncated) = truncate_title_for_subject(commit_type, title);

    let mut subject = format!("{commit_type}: {subject_title} [{}]", task.id);
    for external_ref in &task.external_refs {
        subject.push(' ');
        subject.push_str(&format!(
            "[{}-{}]",
            external_ref.system.to_ascii_uppercase(),
            external_ref.id
        ));
    }

    let mut sections = Vec::new();
    if truncated {
        sections.push(title.to_string());
    }
    if let Some(summary) = execution_summary_paragraph(task) {
        sections.push(summary);
    }
    let trailers = batch_commit_trailers(task);
    if !trailers.is_empty() {
        sections.push(trailers.join("\n"));
    }

    if sections.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{}", sections.join("\n\n"))
    }
}

fn conventional_commit_type(task_type: TaskType) -> &'static str {
    match task_type {
        TaskType::Feature => "feat",
        TaskType::Bug => "fix",
        TaskType::Refactor => "refactor",
        TaskType::Chore => "chore",
    }
}

fn truncate_title_for_subject(commit_type: &str, title: &str) -> (String, bool) {
    let prefix_len = commit_type.chars().count() + ": ".chars().count();
    let title_budget = BATCH_SUBJECT_BUDGET.saturating_sub(prefix_len);
    if title.chars().count() <= title_budget {
        return (title.to_string(), false);
    }

    let retained_chars = title_budget.saturating_sub(1);
    let mut truncated = title.chars().take(retained_chars).collect::<String>();
    truncated.push(ELLIPSIS);
    (truncated, true)
}

fn batch_commit_trailers(task: &Task) -> Vec<String> {
    let mut trailers = Vec::new();
    if let Some(planned_by) = task.planned_by.as_deref() {
        trailers.push(format!("Planned-By: {planned_by}"));
    }
    if let Some(implemented_by) = task.implemented_by.as_deref() {
        trailers.push(format!("Implemented-By: {implemented_by}"));
    }
    if let Some(trailer) = orchestration_trailer(std::slice::from_ref(task)) {
        trailers.push(trailer);
    }
    trailers
}

fn append_orchestration_trailers(message: &mut String, tasks: &[Task]) {
    let Some(trailer) = orchestration_trailer(tasks) else {
        return;
    };

    message.push_str("\n\n");
    message.push_str(&trailer);
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
