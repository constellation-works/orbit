use std::collections::VecDeque;

use orbit_common::OrbitError;
use orbit_search::{
    Embedder, SOURCE_KIND_TASK, SemanticRelatedParams, SemanticSearchParams, bm25_top_k,
};
use orbit_store::friction_store::FrictionListFilter;

use crate::OrbitRuntime;

mod convert;
mod federated;
mod filters;
mod hybrid;
mod path_match;
mod types;

#[cfg(test)]
mod tests;

pub use path_match::task_selectors_contain_path;
pub(crate) use types::empty_whitespace_query_note;
pub use types::{
    GlobalSearchHit, GlobalSearchKind, GlobalSearchMode, GlobalSearchParams, GlobalSearchResponse,
    HitWorkspace, WorkspaceSearchReport,
};

use self::convert::{fill_task_record_fields, lexical_task_hit, semantic_hit_to_global};
use self::filters::{SearchStatusFilters, resolve_task_statuses, task_has_all_tags};
use self::hybrid::{fallback_reason, push_skip_note};

const DEFAULT_LIMIT: usize = 10;
const TASK_HYBRID_FALLBACK_NOTE: &str = "falling back to lexical task search";

#[cfg(test)]
thread_local! {
    static TASK_SEMANTIC_SEARCH_OVERRIDE:
        std::cell::RefCell<Option<Result<Vec<orbit_search::SemanticHit>, OrbitError>>> =
        const { std::cell::RefCell::new(None) };
}

/// The read-only inputs shared by each search-kind branch.
#[derive(Clone, Copy)]
struct BranchContext<'a> {
    params: &'a GlobalSearchParams,
    status_filters: &'a SearchStatusFilters,
    query: Option<&'a str>,
    tag_filter: &'a [String],
    limit: usize,
    /// A query-side embedder the caller already built, or `None` to let each
    /// vector branch build its own.
    ///
    /// A federated read supplies one for the whole fan-out so the companion is
    /// spawned once and the query text is embedded once [DANI-10365].
    embedder: Option<&'a dyn Embedder>,
}

impl OrbitRuntime {
    /// Unified search entry point.
    ///
    /// A `Current` scope — the default — runs the single-workspace path
    /// unchanged. Anything wider fans out through the workspace catalog and
    /// fuses the per-workspace result sets [ORB-11027].
    pub fn global_search(
        &self,
        params: GlobalSearchParams,
    ) -> Result<GlobalSearchResponse, OrbitError> {
        if params.workspaces.is_federated() {
            return self.federated_search(params);
        }
        self.workspace_search(params)
    }

    /// One workspace's answer: this runtime's own checkout, nothing else.
    pub(super) fn workspace_search(
        &self,
        params: GlobalSearchParams,
    ) -> Result<GlobalSearchResponse, OrbitError> {
        self.workspace_search_with(params, None)
    }

    /// [`Self::workspace_search`] reading vectors through a caller-owned
    /// embedder.
    ///
    /// Only the federated fan-out passes one: every workspace in a fan-out
    /// embeds the same query text under the same host model, so a shared
    /// embedder turns N companion spawns and N identical embeddings into one
    /// of each [DANI-10365].
    pub(super) fn workspace_search_with(
        &self,
        params: GlobalSearchParams,
        embedder: Option<&dyn Embedder>,
    ) -> Result<GlobalSearchResponse, OrbitError> {
        let limit = params.normalized_limit();
        let status_filters = SearchStatusFilters::parse(&params.status)?;
        let mut notes = Vec::new();

        if let Some(semantic_id) = params.semantic {
            if params
                .query
                .as_deref()
                .is_some_and(|query| !query.trim().is_empty())
            {
                return Err(OrbitError::InvalidInput(
                    "`query` and `semantic` are mutually exclusive".to_string(),
                ));
            }
            if !matches!(params.kind, GlobalSearchKind::Task | GlobalSearchKind::All) {
                return Err(OrbitError::InvalidInput(
                    "`semantic` only supports --kind task or --kind all".to_string(),
                ));
            }
            let related = self.semantic_related(SemanticRelatedParams {
                task_id: semantic_id,
                limit,
                model: None,
            })?;
            let results = related
                .results
                .into_iter()
                .map(|hit| {
                    let mut global = semantic_hit_to_global(hit);
                    if let Some(id) = global.id.clone()
                        && let Ok(task) = self.get_task(&id)
                    {
                        fill_task_record_fields(&mut global, &task);
                    }
                    global
                })
                .collect();
            return Ok(GlobalSearchResponse {
                mode: GlobalSearchMode::Neighbor,
                kind: params.kind,
                results,
                notes,
                skipped_kinds: Vec::new(),
                workspaces: Vec::new(),
            });
        }

        let query_owned = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_string);
        let has_path = params.path.is_some();
        let tag_filter: Vec<String> = params
            .tags
            .iter()
            .map(|tag| tag.trim().to_lowercase())
            .filter(|tag| !tag.is_empty())
            .collect();

        if query_owned.is_none() && !has_path && tag_filter.is_empty() {
            return Err(OrbitError::InvalidInput(
                "search requires a query, --path, or --tag".to_string(),
            ));
        }

        let mut branches = Vec::new();
        let mut skipped_kinds = Vec::new();
        // Set when a hybrid query's vector branch actually ran — distinct from
        // whether any of its hits survived status/tag filtering, so a hybrid
        // query whose only matches were hidden by the status filter still
        // reports `mode: hybrid` rather than looking like plain lexical search
        // ran [ORB-12259].
        let mut vector_ran = false;

        let branch_context = BranchContext {
            params: &params,
            status_filters: &status_filters,
            query: query_owned.as_deref(),
            tag_filter: &tag_filter,
            limit,
            embedder,
        };

        if params.kind.includes_tasks() {
            branches.push(self.task_branch(branch_context, &mut notes, &mut vector_ran)?);
        }

        if params.kind.includes_frictions() {
            if has_path {
                push_skip_note(
                    &mut notes,
                    "friction",
                    "--path is set; frictions are not path-filtered",
                );
                skipped_kinds.push("friction".to_string());
            } else {
                branches.push(self.friction_branch(
                    &params,
                    &status_filters,
                    query_owned.as_deref(),
                    &tag_filter,
                    limit,
                )?);
            }
        }

        let results = merge_round_robin(branches, limit);
        if results.is_empty()
            && let Some(query) = query_owned.as_deref()
            && let Some(note) = empty_whitespace_query_note(query)
        {
            notes.push(note);
        }
        let mode = if params.hybrid && vector_ran {
            GlobalSearchMode::Hybrid
        } else {
            GlobalSearchMode::Lexical
        };
        Ok(GlobalSearchResponse {
            mode,
            kind: params.kind,
            results,
            notes,
            skipped_kinds,
            workspaces: Vec::new(),
        })
    }

    fn task_branch(
        &self,
        ctx: BranchContext<'_>,
        notes: &mut Vec<String>,
        vector_ran: &mut bool,
    ) -> Result<Vec<GlobalSearchHit>, OrbitError> {
        let BranchContext {
            params,
            status_filters,
            query,
            tag_filter,
            limit,
            embedder,
        } = ctx;
        let statuses = resolve_task_statuses(params, status_filters);

        let candidates = if params.hybrid
            && let Some(query) = query
        {
            let semantic =
                self.task_semantic_hits(query, limit.saturating_mul(2).max(limit), embedder);
            match semantic {
                Ok(hits) if !hits.is_empty() => {
                    *vector_ran = true;
                    hits.into_iter()
                        .map(|hit| {
                            let task = self.get_task(&hit.source_id).ok();
                            (semantic_hit_to_global(hit), task)
                        })
                        .collect()
                }
                Ok(_) => {
                    hybrid::warn_task_hybrid_fallback(notes, "no task embeddings found");
                    self.lexical_task_candidates(query, limit)?
                }
                Err(error) => {
                    let reason = fallback_reason(&error);
                    hybrid::warn_task_hybrid_fallback(notes, &reason);
                    self.lexical_task_candidates(query, limit)?
                }
            }
        } else if let Some(query) = query {
            self.lexical_task_candidates(query, limit)?
        } else {
            // No query → enumerate tasks (used by `--path` and `--tag`).
            let tasks = self.list_tasks()?;
            tasks
                .into_iter()
                .map(|task| (lexical_task_hit(&task), Some(task)))
                .collect()
        };

        let path = params.path.as_deref();

        let mut out = Vec::new();
        let mut hidden_by_status = 0usize;
        for (mut hit, task) in candidates {
            let Some(task) = task else { continue };
            if !statuses.contains(&task.status) {
                if hit.source == "semantic" {
                    hidden_by_status += 1;
                }
                continue;
            }
            if !tag_filter.is_empty() && !task_has_all_tags(&task, tag_filter) {
                continue;
            }
            if let Some(path) = path
                && !task_selectors_contain_path(&task.context_files, path)
            {
                continue;
            }
            // The record is the authority for the fields it owns, so a
            // semantic hit reads like a lexical one instead of carrying an
            // empty title and a stale status.
            fill_task_record_fields(&mut hit, &task);
            out.push(hit);
        }
        out.truncate(limit);
        if *vector_ran && hidden_by_status > 0 {
            notes.push(format!(
                "{hidden_by_status} vector hits hidden by status filter (pass all:true)"
            ));
        }
        Ok(out)
    }

    fn friction_branch(
        &self,
        params: &GlobalSearchParams,
        status_filters: &SearchStatusFilters,
        query: Option<&str>,
        tag_filter: &[String],
        limit: usize,
    ) -> Result<Vec<GlobalSearchHit>, OrbitError> {
        let status = status_filters
            .friction
            .or((!params.all).then_some(orbit_types::record::FrictionStatus::Open));
        let records = crate::runtime::friction::store_for(self)?.list(&FrictionListFilter {
            status,
            q: query.map(str::to_string),
            limit: None,
            ..FrictionListFilter::default()
        })?;

        Ok(records
            .into_iter()
            .filter(|stored| {
                tag_filter.iter().all(|needle| {
                    stored
                        .record
                        .tags
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(needle))
                })
            })
            .take(limit)
            .map(|stored| {
                let record = stored.record;
                GlobalSearchHit {
                    kind: "friction".to_string(),
                    source: "lexical".to_string(),
                    id: Some(record.id.clone()),
                    path: None,
                    title: Some(orbit_common::governance::friction::effective_title(
                        record.title.as_deref(),
                        &record.body,
                        &record.id,
                    )),
                    summary: None,
                    status: Some(record.status.as_str().to_string()),
                    best_field: None,
                    snippet: Some(record.body),
                    score: None,
                    score_breakdown: None,
                    matched_by: None,
                    workspace: None,
                }
            })
            .collect())
    }

    /// Lexical task candidates, at most `2 × limit`, from two sources in order.
    ///
    /// When the semantic index holds task chunks, FTS5 BM25 over
    /// `corpus_fts` ranks first: it matches non-adjacent terms and orders by
    /// relevance. The index carries only title, description, plan,
    /// execution summary, and acceptance criteria, so the bundle matcher then
    /// supplements what FTS cannot see — comments, `external_refs`, artifact
    /// manifest paths, and tasks not yet indexed — appended in index order
    /// behind the BM25 hits [DANI-10445]. Without task chunks the bundle
    /// matcher is the only source. Neither source opens artifact payloads.
    fn lexical_task_candidates(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(GlobalSearchHit, Option<orbit_types::task::Task>)>, OrbitError> {
        let candidate_limit = limit.saturating_mul(2).max(limit);
        let mut seen = std::collections::BTreeSet::new();
        let mut candidates = Vec::with_capacity(candidate_limit);
        if let Ok(index) = self.stores().semantic_index().store()
            && index.has_source_kind(SOURCE_KIND_TASK)?
        {
            // BM25 ranks chunks, while this branch returns tasks. Overfetch by
            // the number of task fields normally indexed, then keep the first
            // occurrence of each task in BM25 order.
            let chunk_limit = candidate_limit.saturating_mul(5).max(candidate_limit);
            for hit in bm25_top_k(index, query, Some(SOURCE_KIND_TASK), None, chunk_limit)? {
                if !seen.insert(hit.source_id.clone()) {
                    continue;
                }
                let task = self.get_task(&hit.source_id).ok();
                if let Some(task) = task {
                    candidates.push((lexical_task_hit(&task), Some(task)));
                }
                if candidates.len() == candidate_limit {
                    return Ok(candidates);
                }
            }
        }

        for task in self.search_tasks_filtered(query, &[])? {
            if candidates.len() == candidate_limit {
                break;
            }
            if !seen.insert(task.id.clone()) {
                continue;
            }
            candidates.push((lexical_task_hit(&task), Some(task)));
        }
        Ok(candidates)
    }

    fn task_semantic_hits(
        &self,
        query: &str,
        limit: usize,
        embedder: Option<&dyn Embedder>,
    ) -> Result<Vec<orbit_search::SemanticHit>, OrbitError> {
        #[cfg(test)]
        if let Some(result) = TASK_SEMANTIC_SEARCH_OVERRIDE.with(|cell| cell.borrow_mut().take()) {
            return result;
        }

        let params = SemanticSearchParams {
            query: query.to_string(),
            limit,
            field: None,
            kind: Some("task".to_string()),
            model: None,
        };
        let store = self.stores().semantic_index().store()?;
        let result = match embedder {
            Some(embedder) => orbit_search::semantic_search_with(store, embedder, params)?,
            None => {
                orbit_search::semantic_search(store, self.stores().semantic_embedders(), params)?
            }
        };
        Ok(result.results)
    }
}

pub(super) fn merge_round_robin(
    branches: Vec<Vec<GlobalSearchHit>>,
    limit: usize,
) -> Vec<GlobalSearchHit> {
    let mut queues = branches
        .into_iter()
        .filter(|branch| !branch.is_empty())
        .map(|branch| branch.into_iter().collect::<VecDeque<_>>())
        .collect::<Vec<_>>();
    let mut out = Vec::with_capacity(limit);

    while out.len() < limit && !queues.is_empty() {
        let mut index = 0;
        while index < queues.len() && out.len() < limit {
            if let Some(hit) = queues[index].pop_front() {
                out.push(hit);
            }
            if queues[index].is_empty() {
                queues.remove(index);
            } else {
                index += 1;
            }
        }
    }

    out
}
