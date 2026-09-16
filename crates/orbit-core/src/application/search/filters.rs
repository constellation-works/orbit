use std::str::FromStr;

use orbit_common::OrbitError;
use orbit_types::record::FrictionStatus;
use orbit_types::task::TaskStatus;

use super::types::GlobalSearchParams;

pub(super) fn task_has_all_tags(task: &orbit_types::task::Task, tag_filter: &[String]) -> bool {
    tag_filter.iter().all(|needle| {
        task.tags
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(needle))
    })
}

/// Whether a doc's tags satisfy every requested tag, case-insensitively.
/// Takes the tags alone because a hybrid candidate's tags may come from the
/// index rather than a walked record [DANI-10369].
pub(super) fn doc_has_all_tags(tags: &[String], tag_filter: &[String]) -> bool {
    tag_filter.iter().all(|needle| {
        tags.iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(needle))
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SearchStatusFilters {
    pub(super) task: Option<Vec<TaskStatus>>,
    pub(super) doc_active: Option<bool>,
    pub(super) friction: Option<FrictionStatus>,
}

impl SearchStatusFilters {
    pub(super) fn parse(raw_statuses: &[String]) -> Result<Self, OrbitError> {
        // ADR-0179: status tokens are kind-qualified to avoid cross-corpus ambiguity.
        let mut filters = Self::default();
        for raw in raw_statuses {
            for token in raw
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
            {
                let Some((kind, value)) = token.split_once(':') else {
                    return Err(OrbitError::InvalidInput(format!(
                        "status token `{token}` must use `kind:value` form"
                    )));
                };
                let kind = kind.trim().to_ascii_lowercase();
                let value = value.trim().to_ascii_lowercase();
                if kind.is_empty() || value.is_empty() {
                    return Err(OrbitError::InvalidInput(format!(
                        "status token `{token}` must use `kind:value` form"
                    )));
                }
                match kind.as_str() {
                    "task" => filters.push_task_status(&value)?,
                    "doc" => filters.set_doc_status(&value)?,
                    "friction" => filters.set_friction_status(&value)?,
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "invalid status kind `{other}` in token `{token}`; expected task, doc, or friction"
                        )));
                    }
                }
            }
        }
        Ok(filters)
    }

    fn push_task_status(&mut self, value: &str) -> Result<(), OrbitError> {
        let statuses = self.task.get_or_insert_with(Vec::new);
        if value == "open" {
            extend_unique(statuses, task_open_statuses());
            return Ok(());
        }
        let status = TaskStatus::from_str(value).map_err(|_| {
            OrbitError::InvalidInput(format!(
                "invalid status `{value}` for kind `task`; expected open, proposed, backlog, in-progress, review, done, blocked, archived, rejected, or someday"
            ))
        })?;
        push_unique(statuses, status);
        Ok(())
    }

    fn set_doc_status(&mut self, value: &str) -> Result<(), OrbitError> {
        if value != "active" {
            return Err(OrbitError::InvalidInput(format!(
                "invalid status `{value}` for kind `doc`; expected active"
            )));
        }
        self.doc_active = Some(true);
        Ok(())
    }

    fn set_friction_status(&mut self, value: &str) -> Result<(), OrbitError> {
        let status = FrictionStatus::from_str(value).map_err(|_| {
            OrbitError::InvalidInput(format!(
                "invalid status `{value}` for kind `friction`; expected open, triaged, or resolved"
            ))
        })?;
        self.friction = Some(status);
        Ok(())
    }
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn extend_unique<T: Copy + PartialEq>(values: &mut Vec<T>, incoming: &[T]) {
    for value in incoming {
        push_unique(values, *value);
    }
}

fn task_open_statuses() -> &'static [TaskStatus] {
    &[
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
    ]
}

pub(super) fn resolve_task_statuses(
    params: &GlobalSearchParams,
    status_filters: &SearchStatusFilters,
) -> Vec<TaskStatus> {
    if let Some(statuses) = &status_filters.task {
        return statuses.clone();
    }
    let mut set = task_open_statuses().to_vec();
    if params.all {
        set.extend([TaskStatus::Done, TaskStatus::Rejected, TaskStatus::Archived]);
    }
    set
}
