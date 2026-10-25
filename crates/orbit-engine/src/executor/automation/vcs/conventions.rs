//! Render a workspace's delivery conventions for one task: the placeholder
//! values PR titles and bodies and commit messages share, and the commit
//! subject budget. Templates were parsed when the configuration loaded, so
//! rendering never meets an unknown placeholder.

use orbit_types::task::{Task, TaskType};
use orbit_types::workflow::{
    COMMIT_SUBJECT_BUDGET, ConventionTemplate, Placeholder, SUBJECT_ELLIPSIS,
};

use crate::context::PrConfig;

use super::freshness::BranchFreshness;
use super::pr::{meaningful_execution_summary, render_acceptance_criteria, task_url};

/// The values one task supplies to a text template. `freshness` is known
/// only when a pull request is opened; elsewhere `{{freshness}}` is empty.
pub(super) struct TaskValues<'a> {
    pub(super) task: &'a Task,
    pub(super) pr_config: &'a PrConfig,
    pub(super) freshness: Option<&'a BranchFreshness>,
}

impl TaskValues<'_> {
    pub(super) fn value(&self, placeholder: &Placeholder) -> String {
        let task = self.task;
        match placeholder {
            Placeholder::TaskId => task.id.clone(),
            Placeholder::Title => task.title.trim().to_string(),
            Placeholder::Type => conventional_commit_type(task.task_type).to_string(),
            Placeholder::Description => task.description.clone(),
            Placeholder::AcceptanceCriteria => {
                render_acceptance_criteria(&task.acceptance_criteria)
            }
            Placeholder::ExecutionSummary => meaningful_execution_summary(&task.execution_summary)
                .unwrap_or_default()
                .to_string(),
            Placeholder::Validation => "- Not reported".to_string(),
            Placeholder::Freshness => self
                .freshness
                .filter(|_| self.pr_config.conventions.freshness_section)
                .map(render_freshness_list)
                .unwrap_or_default(),
            Placeholder::TaskUrl => task_url(task, self.pr_config).unwrap_or_default(),
            Placeholder::ExternalRef(system) => task
                .external_refs
                .iter()
                .find(|external_ref| external_ref.system.eq_ignore_ascii_case(system))
                .map(|external_ref| external_ref.id.clone())
                .unwrap_or_default(),
            // Branch-only placeholders never parse into a text template.
            Placeholder::Prefix | Placeholder::Hash => String::new(),
        }
    }

    pub(super) fn render(&self, template: &ConventionTemplate) -> String {
        template.render(|placeholder| self.value(placeholder))
    }

    /// Render a one-line subject or title: whitespace, newlines included,
    /// collapses to single spaces.
    pub(super) fn render_line(&self, template: &ConventionTemplate) -> String {
        single_line(&self.render(template))
    }

    /// Render a commit subject within [`COMMIT_SUBJECT_BUDGET`] characters.
    /// The title is shortened first, with an ellipsis, so the template's own
    /// text (the task marker included) survives; a subject still over budget
    /// without its title is cut around the complete bracketed task marker,
    /// reserving space for it, so delivery readers never lose the marker.
    /// The flag says whether the title was shortened, so the caller can keep
    /// it in the body.
    pub(super) fn render_subject(&self, template: &ConventionTemplate) -> (String, bool) {
        let subject = self.render_line(template);
        if subject.chars().count() <= COMMIT_SUBJECT_BUDGET {
            return (subject, false);
        }
        let title = self.task.title.trim();
        if template.uses(&Placeholder::Title) {
            let title_chars = title.chars().count();
            for keep in (0..title_chars).rev() {
                let mut shortened = title.chars().take(keep).collect::<String>();
                shortened.push(SUBJECT_ELLIPSIS);
                let candidate = single_line(&template.render(|placeholder| {
                    if *placeholder == Placeholder::Title {
                        shortened.clone()
                    } else {
                        self.value(placeholder)
                    }
                }));
                if candidate.chars().count() <= COMMIT_SUBJECT_BUDGET {
                    return (candidate, true);
                }
            }
        }
        let shortened = template.uses(&Placeholder::Title) && !title.is_empty();
        // Reserve the complete marker: cut the other text around it rather
        // than cutting the whole subject, which could delete the marker
        // a commit subject template is required to carry.
        let marker = format!("[{}]", self.task.id);
        if let Some(start) = subject.find(&marker) {
            let suffix = &subject[start..];
            let suffix_chars = suffix.chars().count();
            let marker_chars = marker.chars().count();
            if marker_chars >= COMMIT_SUBJECT_BUDGET {
                return (marker, shortened);
            }
            if suffix_chars <= COMMIT_SUBJECT_BUDGET.saturating_sub(1) {
                // Add a separator only when the ellipsis leaves room for it.
                let keep = COMMIT_SUBJECT_BUDGET
                    .saturating_sub(suffix_chars)
                    .saturating_sub(2);
                let mut cut: String = subject[..start].chars().take(keep).collect();
                cut = cut.trim_end().to_string();
                cut.push(SUBJECT_ELLIPSIS);
                if suffix_chars < COMMIT_SUBJECT_BUDGET.saturating_sub(1) {
                    cut.push(' ');
                }
                cut.push_str(suffix);
                return (cut, shortened);
            }
            let keep = COMMIT_SUBJECT_BUDGET
                .saturating_sub(marker_chars)
                .saturating_sub(1);
            let mut cut = marker.clone();
            cut.push_str(
                &suffix[marker.len()..]
                    .chars()
                    .take(keep)
                    .collect::<String>(),
            );
            cut.push(SUBJECT_ELLIPSIS);
            return (cut, shortened);
        }
        let mut cut = subject
            .chars()
            .take(COMMIT_SUBJECT_BUDGET.saturating_sub(1))
            .collect::<String>();
        cut.push(SUBJECT_ELLIPSIS);
        (cut, shortened)
    }
}

pub(super) fn conventional_commit_type(task_type: TaskType) -> &'static str {
    match task_type {
        TaskType::Feature => "feat",
        TaskType::Bug => "fix",
        TaskType::Refactor => "refactor",
        TaskType::Chore => "chore",
    }
}

pub(super) fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn render_freshness_list(freshness: &BranchFreshness) -> String {
    format!(
        "- Base ref: `{}`\n- Head ref: `{}`\n- Behind base: {}\n- Ahead of base: {}",
        freshness.base_ref, freshness.head_ref, freshness.commits_behind, freshness.commits_ahead
    )
}
