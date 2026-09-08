use orbit_types::task::Task;

use crate::context::PrConfig;

use super::super::attribution::orchestration_trailer;
use super::super::freshness::BranchFreshness;

pub(super) const GITHUB_PR_BODY_BYTE_LIMIT: usize = 65_536;

/// Builds the generated PR body. One-task PRs use the task-contract-first
/// layout; multi-task callers intentionally keep the historical batch layout
/// until those legacy paths are retired.
pub(super) fn build_batch_pr_body(
    tasks: &[Task],
    freshness: &BranchFreshness,
    changed_files: &[&str],
    pr_config: &PrConfig,
    pr_opener_model: Option<&str>,
) -> String {
    let mut body = if let [task] = tasks {
        build_single_task_pr_body(task, freshness, pr_config)
    } else {
        build_legacy_batch_pr_body(tasks, freshness, changed_files, pr_config)
    };

    if let Some(signature) = batch_pr_signature(tasks, pr_opener_model) {
        body.push_str("\n\n");
        body.push_str(&signature);
    }
    if let Some(trailer) = orchestration_trailer(tasks) {
        body.push_str("\n\n");
        body.push_str(&trailer);
    }

    bound_pr_body(body, tasks)
}

/// Projects a PR body into GitHub's UTF-8 byte limit while keeping the
/// complete task artifacts as the durable source of execution detail.
pub(super) fn bound_pr_body(body: String, tasks: &[Task]) -> String {
    if body.len() <= GITHUB_PR_BODY_BYTE_LIMIT {
        return body;
    }

    let task_ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let durable_record = if task_ids.is_empty() {
        "the persisted Orbit task artifacts".to_string()
    } else {
        format!("the persisted Orbit task artifacts for {task_ids}")
    };
    let audit_note = format!(
        "\n\n> **Audit note:** GitHub limits PR bodies to {GITHUB_PR_BODY_BYTE_LIMIT} bytes. This projection is truncated; complete task artifacts remain the durable record in {durable_record}."
    );
    let retained_body_bytes = GITHUB_PR_BODY_BYTE_LIMIT.saturating_sub(audit_note.len());

    if retained_body_bytes == 0 {
        return truncate_utf8(&audit_note, GITHUB_PR_BODY_BYTE_LIMIT).to_string();
    }

    format!(
        "{}{}",
        truncate_utf8(&body, retained_body_bytes),
        audit_note
    )
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn build_single_task_pr_body(
    task: &Task,
    freshness: &BranchFreshness,
    pr_config: &PrConfig,
) -> String {
    let mut sections = vec![render_single_task_section(task, pr_config)];

    if let Some(execution_summary) = meaningful_execution_summary(&task.execution_summary) {
        sections.push(render_execution_summary_section(execution_summary));
    }

    sections.push(render_validation_section());
    sections.push(render_branch_freshness_section(freshness));
    sections.join("\n\n")
}

pub(super) fn build_legacy_batch_pr_body(
    tasks: &[Task],
    freshness: &BranchFreshness,
    changed_files: &[&str],
    pr_config: &PrConfig,
) -> String {
    let task_sections = tasks
        .iter()
        .map(|task| render_legacy_task_section(task, pr_config))
        .collect::<Vec<_>>()
        .join("\n");
    let changed_files_section = changed_files
        .iter()
        .map(|file| format!("- `{file}`"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Tasks\n{}\n\n## Branch Freshness\n- Base ref: `{}`\n- Head ref: `{}`\n- Behind base: {}\n- Ahead of base: {}\n\n## Files Changed\n{}",
        task_sections,
        freshness.base_ref,
        freshness.head_ref,
        freshness.commits_behind,
        freshness.commits_ahead,
        changed_files_section
    )
}

pub(super) fn render_single_task_section(task: &Task, pr_config: &PrConfig) -> String {
    let acceptance_criteria = render_acceptance_criteria(&task.acceptance_criteria);

    format!(
        "## Task\n\n{}\n\n### Description\n\n{}\n\n### Acceptance Criteria\n\n{}",
        render_single_task_line(task, pr_config),
        task.description,
        acceptance_criteria
    )
}

pub(super) fn render_execution_summary_section(execution_summary: &str) -> String {
    format!(
        "## Execution Summary\n\n<details>\n<summary>Click to expand</summary>\n\n{execution_summary}\n\n</details>"
    )
}

pub(super) fn render_validation_section() -> String {
    "## Validation\n\n- Not reported".to_string()
}

pub(super) fn render_branch_freshness_section(freshness: &BranchFreshness) -> String {
    format!(
        "## Branch Freshness\n\n- Base ref: `{}`\n- Head ref: `{}`\n- Behind base: {}\n- Ahead of base: {}",
        freshness.base_ref, freshness.head_ref, freshness.commits_behind, freshness.commits_ahead
    )
}

pub(super) fn render_acceptance_criteria(criteria: &[String]) -> String {
    criteria
        .iter()
        .map(|criterion| format!("- {criterion}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn render_legacy_task_section(task: &Task, pr_config: &PrConfig) -> String {
    let line = render_task_line(task, pr_config);
    match meaningful_execution_summary(&task.execution_summary) {
        Some(execution_summary) => {
            format!(
                "{line}\n  <details><summary>Execution Summary</summary>\n\n{execution_summary}\n\n  </details>"
            )
        }
        None => line,
    }
}

pub(in crate::executor::automation::vcs) fn meaningful_execution_summary(
    summary: &str,
) -> Option<&str> {
    let trimmed = summary.trim();
    if trimmed.is_empty() || is_placeholder_execution_summary(trimmed) {
        None
    } else {
        Some(trimmed)
    }
}

pub(super) fn is_placeholder_execution_summary(summary: &str) -> bool {
    let normalized = summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let lower = normalized.to_ascii_lowercase();
    let stripped = lower.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    stripped.is_empty()
        || matches!(
            stripped,
            "todo"
                | "tbd"
                | "n/a"
                | "na"
                | "none"
                | "placeholder"
                | "execution summary"
                | "summary"
                | "no execution summary"
                | "no summary provided"
                | "no execution summary provided"
                | "to be authored by executing agent at start time"
        )
}

pub(super) fn render_task_line(task: &Task, pr_config: &PrConfig) -> String {
    let title = task.title.trim();
    let task_ref = render_task_ref(task, pr_config);
    if title.is_empty() {
        format!("- {task_ref}")
    } else {
        format!("- {task_ref} {title}")
    }
}

pub(super) fn render_single_task_line(task: &Task, pr_config: &PrConfig) -> String {
    let title = task.title.trim();
    let task_ref = render_task_ref(task, pr_config);
    if title.is_empty() {
        task_ref
    } else {
        format!("{task_ref} — {title}")
    }
}

pub(super) fn render_task_ref(task: &Task, pr_config: &PrConfig) -> String {
    match task_url(task, pr_config) {
        Some(url) => format!("[{}]({url})", task.id),
        None => task.id.clone(),
    }
}

pub(super) fn task_url(task: &Task, pr_config: &PrConfig) -> Option<String> {
    task.external_refs
        .iter()
        .find_map(|external_ref| external_ref.url.as_deref())
        .map(ToOwned::to_owned)
        .or_else(|| {
            pr_config
                .task_url_template
                .as_ref()
                .map(|template| template.replace("{task_id}", &task.id))
        })
}

pub(super) fn default_pr_title(tasks: &[Task]) -> String {
    let first_task = tasks.first();
    let first_title = first_task
        .map(|task| task.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| first_task.map(|task| task.id.as_str()).unwrap_or("Bundle"));
    if tasks.len() == 1 {
        first_title.to_string()
    } else {
        format!("[Bundle] {first_title}")
    }
}

pub(super) fn batch_pr_signature(tasks: &[Task], pr_opener_model: Option<&str>) -> Option<String> {
    let model = tasks
        .iter()
        .find_map(|task| {
            let model = task.implemented_by.as_deref()?.trim();
            (!model.is_empty()).then_some(model)
        })
        .or_else(|| {
            pr_opener_model
                .map(str::trim)
                .filter(|model| !model.is_empty())
        })?;
    Some(format!("*authored by: {model}*"))
}
