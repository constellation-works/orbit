//! Deterministic delivery highlights and coverage notes for the scoreboard.
//!
//! Highlights are a reading aid built only from durable task fields. They are
//! not an impact rank, quality score, or model-inferred summary.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskPriority, TaskStatus};
use serde::{Deserialize, Serialize};

/// Maximum notable completions shown for one window.
pub const NOTABLE_COMPLETIONS_LIMIT: usize = 5;
/// Maximum characters kept from a task's `execution_summary`.
pub const SUMMARY_EXCERPT_MAX_CHARS: usize = 180;
/// Stable sort key advertised to API/UI consumers.
pub const NOTABLE_SELECTION_METHOD: &str = "priority_then_completion_recency";
/// Operator-facing description of the sort. Not a quality claim.
pub const NOTABLE_SELECTION_LABEL: &str = "Ordered by priority, then most recently completed. This is a reading order, not a quality score.";

/// One completed task selected for the Notable completions list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotableCompletion {
    pub task_id: String,
    pub title: String,
    pub priority: String,
    pub task_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_tag: Option<String>,
    /// RFC3339 completion timestamp (`updated_at` for done/archived tasks).
    pub completed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_excerpt: Option<String>,
}

/// Bounded highlight list plus the documented selection rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotableCompletions {
    pub method: String,
    pub label: String,
    pub limit: usize,
    pub items: Vec<NotableCompletion>,
}

/// Whether a scoreboard section's source can be attributed to the selected window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageAvailability {
    Observed,
    Unavailable,
}

/// Honest source note for one scoreboard section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoverageNote {
    pub availability: CoverageAvailability,
    pub detail: String,
}

/// Per-section coverage for sources that are not uniformly windowable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScoreboardCoverage {
    pub review: CoverageNote,
    pub snapshot_metrics: CoverageNote,
}

impl Default for NotableCompletions {
    fn default() -> Self {
        Self {
            method: NOTABLE_SELECTION_METHOD.to_string(),
            label: NOTABLE_SELECTION_LABEL.to_string(),
            limit: NOTABLE_COMPLETIONS_LIMIT,
            items: Vec::new(),
        }
    }
}

impl Default for ScoreboardCoverage {
    fn default() -> Self {
        snapshot_coverage(false)
    }
}

/// Build the windowed highlight list. Candidates are `done`/`archived` tasks
/// whose completion timestamp falls in `since..`. `since == None` is lifetime.
pub fn select_notable_completions(
    tasks: &[Task],
    since: Option<DateTime<Utc>>,
) -> NotableCompletions {
    let mut candidates: Vec<&Task> = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Done | TaskStatus::Archived))
        .filter(|task| match since {
            None => true,
            Some(cut) => task.updated_at >= cut,
        })
        .collect();

    candidates.sort_by(|a, b| {
        priority_rank(b.priority)
            .cmp(&priority_rank(a.priority))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    candidates.truncate(NOTABLE_COMPLETIONS_LIMIT);

    NotableCompletions {
        method: NOTABLE_SELECTION_METHOD.to_string(),
        label: NOTABLE_SELECTION_LABEL.to_string(),
        limit: NOTABLE_COMPLETIONS_LIMIT,
        items: candidates.into_iter().map(notable_from_task).collect(),
    }
}

/// Snapshot-sourced review/token columns have no per-event timestamp, so a
/// finite window cannot honestly report them as "no activity".
pub fn snapshot_coverage(windowed: bool) -> ScoreboardCoverage {
    if windowed {
        let detail = "Lifetime snapshot with no per-event timestamps; omitted from this window rather than shown as zero activity.".to_string();
        ScoreboardCoverage {
            review: CoverageNote {
                availability: CoverageAvailability::Unavailable,
                detail: detail.clone(),
            },
            snapshot_metrics: CoverageNote {
                availability: CoverageAvailability::Unavailable,
                detail,
            },
        }
    } else {
        ScoreboardCoverage {
            review: CoverageNote {
                availability: CoverageAvailability::Observed,
                detail: "PR comment counts come from the lifetime snapshot. A zero means no observed review comments, not missing coverage.".to_string(),
            },
            snapshot_metrics: CoverageNote {
                availability: CoverageAvailability::Observed,
                detail: "Token and PR snapshot counters are lifetime totals for this view.".to_string(),
            },
        }
    }
}

/// Fill each selected completion's `summary_excerpt` from `execution_summary`,
/// which receives a task id and returns that task's full execution summary.
///
/// For summaries generated from metadata-only tasks (empty execution
/// summaries): selection needs no body, so only the at most
/// [`NOTABLE_COMPLETIONS_LIMIT`] selected tasks pay for a bundle read. The
/// result equals what generating from full tasks would have selected and
/// excerpted.
pub fn fill_notable_summary_excerpts(
    notable: &mut NotableCompletions,
    mut execution_summary: impl FnMut(&str) -> Result<String, OrbitError>,
) -> Result<(), OrbitError> {
    for item in &mut notable.items {
        item.summary_excerpt = excerpt_execution_summary(&execution_summary(&item.task_id)?);
    }
    Ok(())
}

pub fn excerpt_execution_summary(raw: &str) -> Option<String> {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= SUMMARY_EXCERPT_MAX_CHARS {
        return Some(collapsed);
    }
    let mut excerpt: String = collapsed.chars().take(SUMMARY_EXCERPT_MAX_CHARS).collect();
    if let Some(last_space) = excerpt.rfind(' ')
        && last_space >= SUMMARY_EXCERPT_MAX_CHARS / 2
    {
        excerpt.truncate(last_space);
    }
    excerpt.push('…');
    Some(excerpt)
}

fn notable_from_task(task: &Task) -> NotableCompletion {
    NotableCompletion {
        task_id: task.id.to_string(),
        title: task.title.clone(),
        priority: task.priority.to_string(),
        task_type: task.task_type.to_string(),
        impact_tag: explicit_impact_tag(&task.tags),
        completed_at: task.updated_at.to_rfc3339(),
        summary_excerpt: excerpt_execution_summary(&task.execution_summary),
    }
}

/// First tag that starts with `impact:` (case-insensitive). Other tags are
/// ignored so we never invent an impact label from type, priority, or prose.
fn explicit_impact_tag(tags: &[String]) -> Option<String> {
    tags.iter()
        .find(|tag| {
            tag.get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("impact:"))
        })
        .cloned()
}

fn priority_rank(priority: TaskPriority) -> u8 {
    match priority {
        TaskPriority::Critical => 3,
        TaskPriority::High => 2,
        TaskPriority::Medium => 1,
        TaskPriority::Low => 0,
    }
}
