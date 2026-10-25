use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::text::floor_char_boundary;
use orbit_types::task::Task;
use orbit_types::workflow::{ConventionTemplate, KnownRedValidation, TemplateSurface};

use crate::context::PrConfig;

use super::super::attribution::orchestration_trailer;
use super::super::conventions::{TaskValues, render_freshness_list};
use super::super::freshness::BranchFreshness;

pub(super) const GITHUB_PR_BODY_BYTE_LIMIT: usize = 65_536;

/// Setting key named when a body template read at delivery time is refused.
pub(super) const BODY_TEMPLATE_KEY: &str = "pr.body_template";

/// Note published under a required section the implementer left unfilled;
/// the pull request opens as a draft so the heading never stands empty.
pub(super) const MISSING_SECTION_NOTE: &str = "> **Missing required section.** The implementer \
     did not fill this section, so Orbit opened this pull request as a draft.";

/// What a single-task body is rendered from beyond the task: the body
/// template's text, when one applies, and the agent-filled sections the
/// implement step wrote (heading name to Markdown).
#[derive(Default)]
pub(super) struct BodyInputs<'a> {
    pub(super) template: Option<&'a str>,
    pub(super) filled: BTreeMap<String, String>,
}

/// A rendered body and the required sections it published without content.
pub(super) struct RenderedBody {
    pub(super) body: String,
    pub(super) missing_required: Vec<String>,
}

/// Builds the generated PR body. One-task PRs use the task-contract-first
/// layout, or the configured body template; multi-task callers intentionally
/// keep the historical batch layout until those legacy paths are retired.
pub(super) fn build_batch_pr_body(
    tasks: &[Task],
    freshness: &BranchFreshness,
    changed_files: &[&str],
    pr_config: &PrConfig,
    pr_opener_model: Option<&str>,
    inputs: &BodyInputs<'_>,
) -> Result<RenderedBody, OrbitError> {
    let conventions = &pr_config.conventions;
    let (mut body, missing_required) = if let [task] = tasks {
        let values = TaskValues {
            task,
            pr_config,
            freshness: Some(freshness),
        };
        match inputs.template {
            Some(template) => render_template_body(template, &values, inputs)?,
            None => {
                let body = build_single_task_pr_body(task, freshness, pr_config);
                append_agent_sections(body, pr_config, inputs)
            }
        }
    } else {
        (
            build_legacy_batch_pr_body(tasks, freshness, changed_files, pr_config),
            Vec::new(),
        )
    };

    if let Some(signature) =
        batch_pr_signature(tasks, pr_opener_model).filter(|_| conventions.signature)
    {
        body.push_str("\n\n");
        body.push_str(&signature);
    }
    if let Some(trailer) =
        orchestration_trailer(tasks).filter(|_| conventions.pr_orchestration_trailer)
    {
        body.push_str("\n\n");
        body.push_str(&trailer);
    }

    Ok(RenderedBody {
        body: bound_pr_body(body, tasks),
        missing_required,
    })
}

/// The ATX heading a template line opens, as `(level, name)`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let name = rest.trim().trim_end_matches('#').trim();
    Some((level, name))
}

/// Render a body template. Each agent-filled heading keeps its heading line
/// and takes the implementer's content in place of the template text under
/// it (up to the next heading of the same or a higher level); an unfilled
/// optional section keeps the template text, and an unfilled required one
/// gets [`MISSING_SECTION_NOTE`]. Everything else renders its placeholders,
/// and an unknown placeholder is refused naming `pr.body_template`.
fn render_template_body(
    template: &str,
    values: &TaskValues<'_>,
    inputs: &BodyInputs<'_>,
) -> Result<(String, Vec<String>), OrbitError> {
    let conventions = &values.pr_config.conventions;
    let sections = conventions.agent_filled_sections();
    let mut out = String::new();
    let mut missing_required = Vec::new();
    let mut pending = String::new();
    // The level of the agent-filled heading being replaced, while its text
    // is skipped.
    let mut replacing: Option<usize> = None;
    let mut in_fence = false;
    let flush = |pending: &mut String, out: &mut String| -> Result<(), OrbitError> {
        if !pending.is_empty() {
            out.push_str(&render_text(pending, values)?);
            pending.clear();
        }
        Ok(())
    };
    for line in template.split_inclusive('\n') {
        let fence = line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
        if fence {
            in_fence = !in_fence;
        }
        let opened = (!in_fence && !fence).then(|| heading(line)).flatten();
        if let Some(level) = replacing {
            match opened {
                Some((next, _)) if next <= level => replacing = None,
                _ => continue,
            }
        }
        let Some((level, name)) = opened else {
            pending.push_str(line);
            continue;
        };
        let Some(section) = sections
            .iter()
            .find(|section| orbit_types::workflow::same_section_name(section, name))
        else {
            pending.push_str(line);
            continue;
        };
        let content = filled_content(&inputs.filled, section);
        let required = conventions.is_required_section(section);
        if content.is_none() && !required {
            pending.push_str(line);
            continue;
        }
        flush(&mut pending, &mut out)?;
        out.push_str(line.trim_end_matches(['\r', '\n']));
        out.push_str("\n\n");
        match content {
            Some(content) => out.push_str(content),
            None => {
                missing_required.push(section.to_string());
                out.push_str(MISSING_SECTION_NOTE);
            }
        }
        out.push_str("\n\n");
        replacing = Some(level);
    }
    flush(&mut pending, &mut out)?;
    Ok((out.trim_end().to_string(), missing_required))
}

fn render_text(text: &str, values: &TaskValues<'_>) -> Result<String, OrbitError> {
    let template = ConventionTemplate::parse(BODY_TEMPLATE_KEY, text, TemplateSurface::Text)
        .map_err(|error| OrbitError::InvalidInput(format!("pr_open: {error}")))?;
    Ok(values.render(&template))
}

/// Orbit's own layout has no headings to fill, so its agent-filled sections
/// follow it in configured order; an unfilled optional section is left out.
fn append_agent_sections(
    mut body: String,
    pr_config: &PrConfig,
    inputs: &BodyInputs<'_>,
) -> (String, Vec<String>) {
    let conventions = &pr_config.conventions;
    let mut missing_required = Vec::new();
    for section in conventions.agent_filled_sections() {
        let content = match filled_content(&inputs.filled, section) {
            Some(content) => content,
            None if conventions.is_required_section(section) => {
                missing_required.push(section.to_string());
                MISSING_SECTION_NOTE
            }
            None => continue,
        };
        body.push_str(&format!("\n\n## {}\n\n{content}", section.trim()));
    }
    (body, missing_required)
}

fn filled_content<'a>(filled: &'a BTreeMap<String, String>, section: &str) -> Option<&'a str> {
    filled
        .iter()
        .find(|(name, _)| orbit_types::workflow::same_section_name(name, section))
        .map(|(_, content)| content.trim())
        .filter(|content| !content.is_empty())
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
    &value[..floor_char_boundary(value, max_bytes)]
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
    if pr_config.conventions.freshness_section {
        sections.push(render_branch_freshness_section(freshness));
    }
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
    let freshness_section = if pr_config.conventions.freshness_section {
        format!(
            "\n\n## Branch Freshness\n- Base ref: `{}`\n- Head ref: `{}`\n- Behind base: {}\n- Ahead of base: {}",
            freshness.base_ref,
            freshness.head_ref,
            freshness.commits_behind,
            freshness.commits_ahead,
        )
    } else {
        String::new()
    };
    format!(
        "## Tasks\n{task_sections}{freshness_section}\n\n## Files Changed\n{changed_files_section}"
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

/// [ORB-15375] The section naming required validation that failed only with
/// tests the base fails too, or `None` when there is none.
pub(super) fn render_known_red_section(records: &[KnownRedValidation]) -> Option<String> {
    if records.is_empty() {
        return None;
    }
    let commands = records
        .iter()
        .map(|record| {
            let failures = record
                .failure_lines()
                .into_iter()
                .map(|line| format!("  - {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "- `{}` fails on base `{}` with the same tests:\n{failures}",
                record.command, record.base_sha
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "## Known-red validation\n\nThese required validation failures also fail on the base, \
         so this change did not introduce them and was not held for them.\n\n{commands}"
    ))
}

pub(super) fn render_branch_freshness_section(freshness: &BranchFreshness) -> String {
    format!(
        "## Branch Freshness\n\n{}",
        render_freshness_list(freshness)
    )
}

pub(in crate::executor::automation::vcs) fn render_acceptance_criteria(
    criteria: &[String],
) -> String {
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

pub(in crate::executor::automation::vcs) fn task_url(
    task: &Task,
    pr_config: &PrConfig,
) -> Option<String> {
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

/// The pull request title: `pr.title_template` rendered on one line for a
/// single-task delivery, else (or when it renders blank) Orbit's default.
pub(super) fn pr_title(tasks: &[Task], pr_config: &PrConfig) -> String {
    if let ([task], Some(template)) = (tasks, pr_config.conventions.pr_title.as_ref()) {
        let title = TaskValues {
            task,
            pr_config,
            freshness: None,
        }
        .render_line(template);
        if !title.is_empty() {
            return title;
        }
    }
    default_pr_title(tasks)
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

/// Name flaky tests and make their passing isolated replay evidence reviewable.
pub(super) fn render_flaky_section(
    records: &[orbit_types::workflow::FlakyValidation],
) -> Option<String> {
    if records.is_empty() {
        return None;
    }
    let entries = records.iter().map(|record| {
        let tests = record.failures.iter().map(|test| format!("`{}`", test.strip_prefix("test ").unwrap_or(test)))
            .collect::<Vec<_>>().join(", ");
        format!("- `{}` initially failed (exit {}) on `{}`: {tests}. Base `{}` passed; one serial isolated replay passed ({} executed tests).\n\n<details><summary>Captured rerun evidence</summary>\n\n```text\n{}\n```\n\n</details>", record.command, record.original_exit_code, record.tested_head, record.base_sha, record.rerun_tests_run, super::super::required::command::bounded_validation_text(&record.rerun_output, 4096).replace("```", "` ` `"))
    }).collect::<Vec<_>>().join("\n\n");
    Some(format!("## Flaky-test validation\n\n{entries}"))
}
