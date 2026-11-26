//! The fleet task reads: one workspace's share of a page merged across every
//! registered workspace.
//!
//! The aggregate is observational, so selection observes settled envelopes
//! without entering the task-commit boundary: it never queues behind a
//! drain's exclusive commit sections and never triggers recovery or index
//! repair. The `pilot_warnings` selection has no observational form and keeps
//! the coordinated read.

use orbit_common::OrbitError;

use super::{CandidateSection, TaskRows};
use crate::Core;
use crate::application::task::{
    TaskCandidateKey, TaskCandidateKeys, TaskListFilter, TaskListQuery,
};

/// Each section's candidates, as ids and creation times. A filter the task
/// index fully answers is selected there without reading a single envelope;
/// the rest (title search, type, parent, external refs) select over
/// envelopes. A batch selects every section in one indexed read when the
/// index answers. Only a batch whose index read fails fails the command.
pub(super) fn candidates(
    core: &Core,
    sections: &[CandidateSection],
    batch: bool,
) -> Result<Vec<Result<TaskCandidateKeys, OrbitError>>, OrbitError> {
    if batch {
        let filters: Vec<_> = sections
            .iter()
            .map(|section| (section.filter.clone(), section.limit))
            .collect();
        let indexed = core.runtime().observe_task_candidate_key_pages(&filters)?;
        return Ok(indexed.unwrap_or_else(|| {
            filters
                .iter()
                .map(|(filter, limit)| observed_keys(core, filter, *limit))
                .collect()
        }));
    }
    Ok(sections
        .iter()
        .map(|section| section_keys(core, section))
        .collect())
}

fn section_keys(core: &Core, section: &CandidateSection) -> Result<TaskCandidateKeys, OrbitError> {
    let runtime = core.runtime();
    if section.pilot_warnings {
        let page = runtime.query_task_rows(&TaskListQuery {
            filter: section.filter.clone(),
            pilot_warnings: true,
            limit: section.limit,
            ..Default::default()
        })?;
        return Ok(TaskCandidateKeys {
            items: page
                .items
                .into_iter()
                .map(|row| TaskCandidateKey {
                    id: row.task.id,
                    created_at: row.task.created_at,
                })
                .collect(),
            total: page.total,
            total_without_cursor: page.total_without_cursor,
        });
    }
    if let Some(keys) = runtime.observe_task_candidate_keys(&section.filter, section.limit)? {
        return Ok(keys);
    }
    observed_keys(core, &section.filter, section.limit)
}

fn observed_keys(
    core: &Core,
    filter: &TaskListFilter,
    limit: usize,
) -> Result<TaskCandidateKeys, OrbitError> {
    let page = core.runtime().observe_task_candidates(filter, limit)?;
    Ok(TaskCandidateKeys {
        items: page
            .items
            .into_iter()
            .map(|task| TaskCandidateKey {
                id: task.id,
                created_at: task.created_at,
            })
            .collect(),
        total: page.total,
        total_without_cursor: page.total_without_cursor,
    })
}

/// The listed rows of `ids`, in order, and one batched delivery-run read
/// over the rows that read.
pub(super) fn rows(core: &Core, ids: &[String]) -> TaskRows {
    let runtime = core.runtime();
    let rows: Vec<_> = ids
        .iter()
        .map(|id| runtime.get_listed_task_row(id))
        .collect();
    let read = || {
        rows.iter()
            .filter_map(|row| row.as_ref().ok().and_then(Option::as_ref))
    };
    let (delivery_attention, run_branches) = if read().next().is_some() {
        (
            runtime.delivery_attention_map(read()),
            runtime.local_run_branches(read()),
        )
    } else {
        (Ok(Default::default()), Default::default())
    };
    TaskRows {
        rows,
        delivery_attention,
        run_branches,
    }
}
