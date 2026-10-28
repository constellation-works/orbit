use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::{
    MissingSectionPolicy, PR_SECTION_GAP_EVENT, PR_SECTIONS_ARTIFACT, PrBodyTemplateSource,
    REPO_PR_TEMPLATE_LOCATIONS, SCREENSHOT_ARTIFACT_PREFIX, SCREENSHOTS_SECTION,
};

use crate::context::{PrConfig, RuntimeHost, TaskAutomationUpdate};

use super::super::super::git::git_output_raw;
use super::super::super::handoff::HandoffContext;
use super::super::body::BODY_TEMPLATE_KEY;

/// The text of the configured body template for a single-task delivery, as
/// committed at `base_sha`, so a delivery cannot rewrite the template it is
/// published with. `None` keeps Orbit's layout: no template is configured,
/// the delivery carries several tasks, or `"repo"` finds no template.
pub(super) fn body_template_text(
    workspace_path: &std::path::Path,
    tasks: &[Task],
    pr_config: &PrConfig,
    base_sha: &str,
) -> Result<Option<String>, OrbitError> {
    let (Some(source), [_]) = (pr_config.conventions.pr_body.as_ref(), tasks) else {
        return Ok(None);
    };
    let path = match source {
        PrBodyTemplateSource::Path(path) => path.clone(),
        PrBodyTemplateSource::Repository => {
            match repository_template_path(workspace_path, base_sha)? {
                Some(path) => path,
                None => {
                    tracing::info!(
                        base_sha,
                        "pr.body_template = \"repo\" found no pull request template at the base; \
                     using Orbit's layout"
                    );
                    return Ok(None);
                }
            }
        }
    };
    let text = git_output_raw(workspace_path, &["show", &format!("{base_sha}:{path}")]).map_err(
        |error| {
            OrbitError::InvalidInput(format!(
                "pr_open: {BODY_TEMPLATE_KEY}: cannot read '{path}' at base {base_sha}: {error}"
            ))
        },
    )?;
    Ok(Some(text))
}

/// The repository's GitHub pull request template at `base_sha`: the first of
/// [`REPO_PR_TEMPLATE_LOCATIONS`] that names a file there, matched
/// case-insensitively as GitHub does.
fn repository_template_path(
    workspace_path: &std::path::Path,
    base_sha: &str,
) -> Result<Option<String>, OrbitError> {
    for (directory, name) in REPO_PR_TEMPLATE_LOCATIONS {
        let tree = if directory.is_empty() {
            base_sha.to_string()
        } else {
            format!("{base_sha}:{directory}")
        };
        // A missing directory is not an error: the next location is tried.
        let Ok(listing) = git_output_raw(workspace_path, &["ls-tree", "-z", &tree]) else {
            continue;
        };
        for entry in listing.split('\0') {
            let Some((meta, file)) = entry.split_once('\t') else {
                continue;
            };
            if meta.split_whitespace().nth(1) == Some("blob") && file.eq_ignore_ascii_case(name) {
                return Ok(Some(if directory.is_empty() {
                    file.to_string()
                } else {
                    format!("{directory}/{file}")
                }));
            }
        }
    }
    Ok(None)
}

/// The agent-filled sections the implement step wrote to the task's
/// [`PR_SECTIONS_ARTIFACT`]: `{"sections": {"<heading>": "<markdown>"}}`.
/// Only a single-task delivery with agent-filled sections configured reads
/// it. A missing or malformed artifact fills nothing, so required sections
/// surface as gaps rather than as a failed step.
pub(super) fn filled_sections<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[Task],
    pr_config: &PrConfig,
) -> BTreeMap<String, String> {
    let [task] = tasks else {
        return BTreeMap::new();
    };
    if pr_config.conventions.agent_filled_sections().is_empty() {
        return BTreeMap::new();
    }
    let artifact = match host.get_task_artifact(&task.id, PR_SECTIONS_ARTIFACT) {
        Ok(Some(artifact)) => artifact,
        Ok(None) => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(
                task_id = task.id,
                "pr_open could not read {PR_SECTIONS_ARTIFACT}: {error}"
            );
            return BTreeMap::new();
        }
    };
    #[derive(serde::Deserialize)]
    struct Sections {
        sections: BTreeMap<String, String>,
    }
    match serde_json::from_slice::<Sections>(&artifact.content) {
        Ok(parsed) => parsed.sections,
        Err(error) => {
            tracing::warn!(
                task_id = task.id,
                "pr_open ignored a malformed {PR_SECTIONS_ARTIFACT}: {error}"
            );
            BTreeMap::new()
        }
    }
}

/// What is missing behind each unfilled required section: agent-filled ones
/// lack content in [`PR_SECTIONS_ARTIFACT`], and the screenshot section lacks
/// image artifacts under [`SCREENSHOT_ARTIFACT_PREFIX`].
pub(super) fn section_gaps(missing: &[String]) -> String {
    let (screenshots, agent): (Vec<String>, Vec<String>) = missing
        .iter()
        .cloned()
        .partition(|section| section == SCREENSHOTS_SECTION);
    let mut gaps = Vec::new();
    if !agent.is_empty() {
        gaps.push(format!(
            "required pull request section(s) {} have no filled content in task artifact \
             `{PR_SECTIONS_ARTIFACT}`",
            quoted_sections(&agent)
        ));
    }
    if !screenshots.is_empty() {
        gaps.push(format!(
            "required pull request section `{SCREENSHOTS_SECTION}` has no image task artifacts \
             under `{SCREENSHOT_ARTIFACT_PREFIX}`"
        ));
    }
    gaps.join("; ")
}

fn quoted_sections(sections: &[String]) -> String {
    sections
        .iter()
        .map(|section| format!("`{section}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Record in each task's history which required sections had no content and
/// what delivery did about it.
pub(super) fn record_section_gap<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    missing: &[String],
    pr_config: &PrConfig,
) -> Result<(), OrbitError> {
    let action = match pr_config.conventions.missing_section {
        MissingSectionPolicy::Draft => "opens the pull request as a draft",
        MissingSectionPolicy::Refuse => "refuses to open the pull request",
    };
    let gaps = section_gaps(missing);
    let note = format!(
        "{}{}; pr_open {action} (pr.missing_required_section = \"{}\").",
        gaps[..1].to_ascii_uppercase(),
        &gaps[1..],
        pr_config.conventions.missing_section.as_str()
    );
    for task in &context.tasks {
        host.apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".to_string(),
                    event: PR_SECTION_GAP_EVENT.to_string(),
                    note: Some(note.clone()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}
