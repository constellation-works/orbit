//! Aggregate fleet task listing across registered workspaces.
//!
//! Candidate selection is observational and runs concurrently across opened
//! workspaces with bounded fan-out, observing settled envelopes without
//! entering the task-commit boundary (never queueing behind a drain's
//! exclusive commit sections and never triggering recovery or index repair).
//! The `pilot_warnings` selection has no observational form and keeps the
//! coordinated read. Surviving page rows are then hydrated per workspace in
//! page order, and the shared coordination registry's dependency statuses are
//! read once per non-empty section.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use orbit_common::OrbitError;
use serde_json::Value;

use super::{
    AggregateCursorAnchor, AggregateTaskPage, AggregateTaskPages, AggregateTaskRequest,
    AggregateTaskRow, AggregateTaskSection, AggregateWorkspaceSource, UnavailableWorkspace,
};
use crate::Core;
use crate::application::task::{
    TaskCandidateKey, TaskCandidateKeys, TaskListFilter, TaskListQuery, TaskRow,
};

/// Upper bound on the scoped threads one aggregate task read fans out to.
/// Each workspace read is independent (its own runtime and store), so the
/// aggregate waits for the slowest workspace instead of the sum while keeping
/// a host with many registered workspaces from spawning a thread per workspace
/// per poll.
const MAX_AGGREGATE_FAN_OUT: usize = 8;

/// Selected candidates and accounting for one section before row hydration.
struct SelectedSection {
    candidates: Vec<(TaskCandidateKey, usize)>,
    total: usize,
    candidate_count: usize,
    next_cursor: Option<AggregateCursorAnchor>,
    unavailable: Vec<UnavailableWorkspace>,
    unavailable_indices: HashSet<usize>,
}

/// Hydrated rows and batched per-workspace metadata for one workspace's slice.
struct WorkspaceRowBatch {
    rows: Vec<Result<Option<TaskRow>, OrbitError>>,
    delivery_attention: Result<BTreeMap<String, Value>, OrbitError>,
    run_branches: BTreeMap<String, String>,
}

/// Run one aggregate task listing across all requested workspaces and sections.
pub(super) fn aggregate(
    bound: &Core,
    request: &AggregateTaskRequest<'_>,
) -> Result<AggregateTaskPages, OrbitError> {
    if request.sections.is_empty() {
        return Ok(AggregateTaskPages {
            sections: Vec::new(),
        });
    }
    let selections = select_candidates(bound, request);
    let sections = request
        .sections
        .iter()
        .zip(selections)
        .map(|(section, section_selections)| {
            let selected =
                select_section_candidates(section_selections, request.resume_index, section.limit);
            hydrate_section_rows(bound, request, selected)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AggregateTaskPages { sections })
}

/// Visit each opened workspace concurrently, then transpose the per-workspace
/// section results into section order. Within each section, workspace order
/// and individual errors stay independent of worker completion order.
fn select_candidates(
    bound: &Core,
    request: &AggregateTaskRequest<'_>,
) -> Vec<Vec<Result<TaskCandidateKeys, String>>> {
    let batch_indexed = request.sections.len() > 1
        && request
            .sections
            .iter()
            .all(|section| !section.pilot_warnings);
    let opened_indices: Vec<usize> = request
        .workspaces
        .iter()
        .enumerate()
        .filter_map(|(index, workspace)| {
            workspace_core(bound, &workspace.source).ok().map(|_| index)
        })
        .collect();
    let opened_results = fan_out(opened_indices.len(), |slot| {
        let index = opened_indices[slot];
        let workspace = &request.workspaces[index];
        let core = match workspace_core(bound, &workspace.source) {
            Ok(core) => core,
            Err(error) => return Err(error.to_string()),
        };
        workspace_candidates(
            core,
            workspace.id,
            &request.sections,
            index,
            request.resume_index,
            batch_indexed,
        )
    });
    transpose_selections(&request.workspaces, request.sections.len(), opened_results)
}

fn transpose_selections(
    workspaces: &[super::AggregateWorkspace<'_>],
    section_count: usize,
    opened_results: Vec<Result<Vec<Result<TaskCandidateKeys, String>>, String>>,
) -> Vec<Vec<Result<TaskCandidateKeys, String>>> {
    let mut opened_iter = opened_results.into_iter();
    let mut by_section: Vec<_> = (0..section_count)
        .map(|_| Vec::with_capacity(workspaces.len()))
        .collect();
    for workspace in workspaces {
        let selection = match &workspace.source {
            AggregateWorkspaceSource::Unavailable(error) => Err(error.clone()),
            AggregateWorkspaceSource::Bound | AggregateWorkspaceSource::Peer(_) => opened_iter
                .next()
                .unwrap_or_else(|| Err("missing workspace candidate selection".to_string())),
        };
        match selection {
            Ok(pages) => {
                for (section, page) in by_section.iter_mut().zip(pages) {
                    section.push(page);
                }
            }
            Err(error) => {
                for section in &mut by_section {
                    section.push(Err(error.clone()));
                }
            }
        }
    }
    by_section
}

/// Select one workspace's candidates for every requested section. A batch
/// reads every section in one indexed pass when the index answers; only a
/// batch whose index read fails skips the whole workspace across all sections.
fn workspace_candidates(
    core: &Core,
    workspace_id: &str,
    sections: &[AggregateTaskSection],
    workspace_index: usize,
    resume_index: usize,
    batch_indexed: bool,
) -> Result<Vec<Result<TaskCandidateKeys, String>>, String> {
    let filters: Vec<_> = sections
        .iter()
        .map(|section| section_filter(section, workspace_index, resume_index))
        .collect();
    let pages = if batch_indexed {
        let indexed = core
            .runtime()
            .observe_task_candidate_key_pages(&filters)
            .map_err(|error| {
                tracing::warn!(
                    workspace = %workspace_id,
                    %error,
                    "fleet task list skipped a workspace"
                );
                error.to_string()
            })?;
        indexed.unwrap_or_else(|| {
            filters
                .iter()
                .map(|(filter, limit)| observed_keys(core, filter, *limit))
                .collect()
        })
    } else {
        sections
            .iter()
            .zip(filters)
            .map(|(section, (filter, limit))| {
                section_keys(core, &filter, limit, section.pilot_warnings)
            })
            .collect()
    };
    Ok(pages
        .into_iter()
        .map(|page| {
            page.map_err(|error| {
                tracing::warn!(
                    workspace = %workspace_id,
                    %error,
                    "fleet task list skipped a workspace"
                );
                error.to_string()
            })
        })
        .collect())
}

fn section_filter(
    section: &AggregateTaskSection,
    workspace_index: usize,
    resume_index: usize,
) -> (TaskListFilter, usize) {
    match workspace_index.cmp(&resume_index) {
        // Only the total is read from a spent workspace.
        Ordering::Less => (without_cursor(&section.filter), 1),
        Ordering::Equal => (section.filter.clone(), section.limit),
        Ordering::Greater => (without_cursor(&section.filter), section.limit),
    }
}

fn without_cursor(filter: &TaskListFilter) -> TaskListFilter {
    TaskListFilter {
        scan_before: None,
        ..filter.clone()
    }
}

fn section_keys(
    core: &Core,
    filter: &TaskListFilter,
    limit: usize,
    pilot_warnings: bool,
) -> Result<TaskCandidateKeys, OrbitError> {
    let runtime = core.runtime();
    if pilot_warnings {
        let page = runtime.query_task_rows(&TaskListQuery {
            filter: filter.clone(),
            pilot_warnings: true,
            limit,
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
    if let Some(keys) = runtime.observe_task_candidate_keys(filter, limit)? {
        return Ok(keys);
    }
    observed_keys(core, filter, limit)
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

/// Merge per-workspace candidates in workspace order, truncate to `limit`,
/// and capture the continuation anchor before row hydration.
fn select_section_candidates(
    selections: Vec<Result<TaskCandidateKeys, String>>,
    resume_index: usize,
    limit: usize,
) -> SelectedSection {
    let mut candidates = Vec::new();
    let mut total = 0;
    let mut remaining = 0;
    let mut unavailable = Vec::new();
    let mut unavailable_indices = HashSet::new();
    for (index, selection) in selections.into_iter().enumerate() {
        let page = match selection {
            Ok(page) => page,
            Err(error) => {
                record_unavailable(&mut unavailable, &mut unavailable_indices, index, error);
                continue;
            }
        };
        total += page.total_without_cursor;
        if index < resume_index {
            continue;
        }
        remaining += page.total;
        let mut items = page.items;
        items.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        for task in items {
            candidates.push((task, index));
        }
    }
    candidates.truncate(limit);
    let candidate_count = candidates.len();
    let next_cursor = if remaining > candidate_count {
        candidates
            .last()
            .map(|(task, workspace_index)| AggregateCursorAnchor {
                workspace_index: *workspace_index,
                created_at: task.created_at,
                task_id: task.id.clone(),
            })
    } else {
        None
    };
    SelectedSection {
        candidates,
        total,
        candidate_count,
        next_cursor,
        unavailable,
        unavailable_indices,
    }
}

/// Hydrate surviving candidates per workspace in page order, read the shared
/// dependency status index once, and propagate the first delivery-attention
/// read error in page order.
fn hydrate_section_rows(
    bound: &Core,
    request: &AggregateTaskRequest<'_>,
    mut selected: SelectedSection,
) -> Result<AggregateTaskPage, OrbitError> {
    let groups = group_candidates_by_workspace(std::mem::take(&mut selected.candidates));
    let mut rows = Vec::new();
    let mut attention_by_workspace = Vec::new();
    let mut run_branches = BTreeMap::new();
    for (workspace_index, ids) in groups {
        let workspace = &request.workspaces[workspace_index];
        let core = workspace_core(bound, &workspace.source)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        let batch = hydrate_workspace_rows(core, &ids);
        let read_any = collect_workspace_rows(
            workspace_index,
            workspace.id,
            &ids,
            batch.rows,
            &mut rows,
            &mut selected,
        );
        if read_any {
            attention_by_workspace.push((workspace_index, batch.delivery_attention));
            run_branches.insert(workspace_index, batch.run_branches);
        }
    }
    let dependency_statuses = read_dependency_statuses(bound, request, &rows)?;
    let delivery_attention = attention_by_workspace
        .into_iter()
        .map(|(workspace_index, attention)| attention.map(|attention| (workspace_index, attention)))
        .collect::<Result<BTreeMap<usize, BTreeMap<String, Value>>, OrbitError>>()?;
    Ok(AggregateTaskPage {
        rows,
        total: selected.total,
        candidate_count: selected.candidate_count,
        next_cursor: selected.next_cursor,
        delivery_attention,
        run_branches,
        dependency_statuses,
        unavailable: selected.unavailable,
    })
}

fn collect_workspace_rows(
    workspace_index: usize,
    workspace_id: &str,
    ids: &[String],
    read_rows: Vec<Result<Option<TaskRow>, OrbitError>>,
    rows: &mut Vec<AggregateTaskRow>,
    selected: &mut SelectedSection,
) -> bool {
    let mut read_any = false;
    for (id, row) in ids.iter().zip(read_rows) {
        match row {
            Ok(Some(row)) => {
                read_any = true;
                rows.push(AggregateTaskRow {
                    workspace_index,
                    row,
                });
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    workspace = %workspace_id,
                    task = %id,
                    %error,
                    "fleet task list skipped an unreadable task row"
                );
                record_unavailable(
                    &mut selected.unavailable,
                    &mut selected.unavailable_indices,
                    workspace_index,
                    error.to_string(),
                );
            }
        }
    }
    read_any
}

fn read_dependency_statuses(
    bound: &Core,
    request: &AggregateTaskRequest<'_>,
    rows: &[AggregateTaskRow],
) -> Result<BTreeMap<String, orbit_types::task::TaskStatus>, OrbitError> {
    let Some(first_row) = rows.first() else {
        return Ok(BTreeMap::new());
    };
    let core = workspace_core(bound, &request.workspaces[first_row.workspace_index].source)
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    core.runtime()
        .dependency_status_index(rows.iter().map(|entry| &entry.row.task))
}

fn group_candidates_by_workspace(
    candidates: Vec<(TaskCandidateKey, usize)>,
) -> Vec<(usize, Vec<String>)> {
    let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
    for (task, workspace_index) in candidates {
        match groups.last_mut() {
            Some((last_index, ids)) if *last_index == workspace_index => ids.push(task.id),
            _ => groups.push((workspace_index, vec![task.id])),
        }
    }
    groups
}

/// The listed rows of `ids`, in order, and one batched delivery-run read
/// over the rows that read.
fn hydrate_workspace_rows(core: &Core, ids: &[String]) -> WorkspaceRowBatch {
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
        (Ok(BTreeMap::new()), BTreeMap::new())
    };
    WorkspaceRowBatch {
        rows,
        delivery_attention,
        run_branches,
    }
}

fn workspace_core<'a>(
    bound: &'a Core,
    source: &'a AggregateWorkspaceSource<'a>,
) -> Result<&'a Core, &'a str> {
    match source {
        AggregateWorkspaceSource::Bound => Ok(bound),
        AggregateWorkspaceSource::Peer(core) => Ok(core),
        AggregateWorkspaceSource::Unavailable(error) => Err(error.as_str()),
    }
}

/// First recording wins so a workspace with several unreadable rows is named once.
fn record_unavailable(
    unavailable: &mut Vec<UnavailableWorkspace>,
    recorded: &mut HashSet<usize>,
    workspace_index: usize,
    error: String,
) {
    if recorded.insert(workspace_index) {
        unavailable.push(UnavailableWorkspace {
            workspace_index,
            error,
        });
    }
}

/// Run `work(index)` for every index in `0..count` on up to
/// [`MAX_AGGREGATE_FAN_OUT`] scoped threads, returning results in index order.
/// Each worker claims the next unclaimed index so one slow workspace delays no
/// other. A panicking worker re-raises in the caller.
fn fan_out<T>(count: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T>
where
    T: Send,
{
    if count <= 1 {
        return (0..count).map(work).collect();
    }
    let workers = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .min(MAX_AGGREGATE_FAN_OUT)
        .min(count);
    let next_index = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut claimed = Vec::new();
                    loop {
                        let index = next_index.fetch_add(1, AtomicOrdering::Relaxed);
                        if index >= count {
                            return claimed;
                        }
                        claimed.push((index, work(index)));
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(count);
        for handle in handles {
            match handle.join() {
                Ok(claimed) => results.extend(claimed),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    })
}
