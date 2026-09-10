use std::collections::{BTreeMap, VecDeque};

use orbit_common::OrbitError;
use orbit_search::{
    DocSemanticHit, DocSemanticSearchParams, SemanticRelatedParams, SemanticSearchParams,
};
use orbit_store::friction_store::FrictionListFilter;

use crate::OrbitRuntime;
use crate::application::docs::SearchResult;

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

use self::convert::{doc_result_to_global, lexical_task_hit, semantic_hit_to_global};
use self::filters::{
    SearchStatusFilters, doc_has_all_tags, resolve_task_statuses, task_has_all_tags,
};
use self::hybrid::{
    DocHybridCandidate, blend_doc_hybrid_candidates, compare_global_hits_by_score,
    doc_search_candidate_limit, fallback_reason, lexical_doc_hits, push_skip_note,
    warn_doc_hybrid_fallback,
};

const DEFAULT_LIMIT: usize = 10;
const DOC_SEARCH_OVERFETCH: usize = 4;
const DOC_HYBRID_FALLBACK_NOTE: &str = "falling back to lexical doc search";
const TASK_HYBRID_FALLBACK_NOTE: &str = "falling back to lexical task search";
const DOC_SEARCH_MIN_CANDIDATES: usize = DEFAULT_LIMIT * DOC_SEARCH_OVERFETCH;

#[cfg(test)]
thread_local! {
    static DOC_SEMANTIC_SEARCH_OVERRIDE:
        std::cell::RefCell<Option<Result<Vec<DocSemanticHit>, OrbitError>>> =
        const { std::cell::RefCell::new(None) };
    static TASK_SEMANTIC_SEARCH_OVERRIDE:
        std::cell::RefCell<Option<Result<Vec<orbit_search::SemanticHit>, OrbitError>>> =
        const { std::cell::RefCell::new(None) };
}

#[derive(Debug, Clone, Copy)]
struct HybridSearchScope<'a> {
    tag_filter: &'a [String],
    limit: usize,
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
                .map(semantic_hit_to_global)
                .collect();
            return Ok(GlobalSearchResponse {
                mode: GlobalSearchMode::Neighbor,
                kind: params.kind,
                results,
                notes,
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

        if params.kind.includes_tasks() {
            branches.push(self.task_branch(
                &params,
                &status_filters,
                query_owned.as_deref(),
                &tag_filter,
                limit,
                &mut notes,
            )?);
        }

        if params.kind.includes_docs() {
            if has_path {
                push_skip_note(
                    &mut notes,
                    "doc",
                    "--path is set; docs are not path-filtered yet",
                );
            } else {
                branches.push(self.doc_branch(
                    &params,
                    &status_filters,
                    query_owned.as_deref(),
                    &tag_filter,
                    limit,
                    &mut notes,
                )?);
            }
        }

        if params.kind.includes_frictions() {
            if has_path {
                push_skip_note(
                    &mut notes,
                    "friction",
                    "--path is set; frictions are not path-filtered",
                );
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
        let mode = if params.hybrid
            && results
                .iter()
                .any(|hit| matches!(hit.source.as_str(), "hybrid" | "semantic"))
        {
            GlobalSearchMode::Hybrid
        } else {
            GlobalSearchMode::Lexical
        };
        Ok(GlobalSearchResponse {
            mode,
            kind: params.kind,
            results,
            notes,
            workspaces: Vec::new(),
        })
    }

    fn task_branch(
        &self,
        params: &GlobalSearchParams,
        status_filters: &SearchStatusFilters,
        query: Option<&str>,
        tag_filter: &[String],
        limit: usize,
        notes: &mut Vec<String>,
    ) -> Result<Vec<GlobalSearchHit>, OrbitError> {
        let statuses = resolve_task_statuses(params, status_filters);

        let candidates = if params.hybrid
            && let Some(query) = query
        {
            let semantic = self.task_semantic_hits(query, limit.saturating_mul(2).max(limit));
            match semantic {
                Ok(hits) if !hits.is_empty() => hits
                    .into_iter()
                    .map(|hit| {
                        let task = self.get_task(&hit.source_id).ok();
                        (semantic_hit_to_global(hit), task)
                    })
                    .collect(),
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
        for (mut hit, task) in candidates {
            let Some(task) = task else { continue };
            if !statuses.contains(&task.status) {
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
            // Override status to keep semantic hits coherent.
            hit.status = Some(task.status.to_string());
            out.push(hit);
        }
        out.truncate(limit);
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

    fn lexical_task_candidates(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(GlobalSearchHit, Option<orbit_types::task::Task>)>, OrbitError> {
        let mut tasks = self.search_tasks_filtered(query, &[])?;
        tasks.truncate(limit.saturating_mul(2).max(limit));
        Ok(tasks
            .into_iter()
            .map(|task| (lexical_task_hit(&task), Some(task)))
            .collect())
    }

    fn task_semantic_hits(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<orbit_search::SemanticHit>, OrbitError> {
        #[cfg(test)]
        if let Some(result) = TASK_SEMANTIC_SEARCH_OVERRIDE.with(|cell| cell.borrow_mut().take()) {
            return result;
        }

        Ok(self
            .semantic_search(SemanticSearchParams {
                query: query.to_string(),
                limit,
                field: None,
                kind: Some("task".to_string()),
                model: None,
            })?
            .results)
    }

    fn doc_branch(
        &self,
        params: &GlobalSearchParams,
        status_filters: &SearchStatusFilters,
        query: Option<&str>,
        tag_filter: &[String],
        limit: usize,
        notes: &mut Vec<String>,
    ) -> Result<Vec<GlobalSearchHit>, OrbitError> {
        let _doc_status_active = status_filters.doc_active.unwrap_or(true);
        let Some(query) = query else {
            if tag_filter.is_empty() {
                // Without a query or tag filter, no doc results — docs are
                // content-indexed, not applicability-indexed.
                return Ok(Vec::new());
            }
            let mut out = Vec::new();
            for record in self.list_docs(None, None)? {
                if !doc_has_all_tags(&record, tag_filter) {
                    continue;
                }
                out.push(GlobalSearchHit {
                    kind: "doc".to_string(),
                    source: "lexical".to_string(),
                    id: None,
                    path: Some(record.path),
                    title: None,
                    summary: Some(record.frontmatter.summary),
                    status: Some(record.frontmatter.doc_type.as_str().to_string()),
                    best_field: None,
                    snippet: None,
                    score: None,
                    score_breakdown: None,
                    matched_by: Some(tag_filter.iter().map(|tag| format!("tag:{tag}")).collect()),
                    workspace: None,
                });
            }
            out.truncate(limit);
            return Ok(out);
        };

        let docs_limit = doc_search_candidate_limit(limit);
        let docs = self.search_docs(query, Some(docs_limit), true)?;
        if params.hybrid {
            // ADR-0180: doc vectors are opt-in and fall back to lexical rather than failing user search.
            return self.hybrid_doc_hits(
                query,
                docs,
                HybridSearchScope { tag_filter, limit },
                notes,
            );
        }

        let mut out = Vec::new();
        for result in docs {
            let SearchResult::Doc(result) = result;
            if !tag_filter.is_empty() {
                let record_tags = &result.record.tags;
                if !tag_filter.iter().all(|tag| {
                    record_tags
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(tag))
                }) {
                    continue;
                }
            }
            let score = result.score as f32;
            out.push(doc_result_to_global(result, "lexical", Some(score)));
        }
        out.truncate(limit);
        Ok(out)
    }

    fn hybrid_doc_hits(
        &self,
        query: &str,
        lexical_results: Vec<SearchResult>,
        scope: HybridSearchScope<'_>,
        notes: &mut Vec<String>,
    ) -> Result<Vec<GlobalSearchHit>, OrbitError> {
        let docs_limit = doc_search_candidate_limit(scope.limit);
        let mut lexical_docs = Vec::<orbit_search::DocSearchResult>::new();
        for result in lexical_results {
            match result {
                SearchResult::Doc(result) => {
                    if !scope.tag_filter.is_empty()
                        && !scope.tag_filter.iter().all(|tag| {
                            result
                                .record
                                .tags
                                .iter()
                                .any(|candidate| candidate.eq_ignore_ascii_case(tag))
                        })
                    {
                        continue;
                    }
                    lexical_docs.push(result);
                }
            }
        }

        let lexical_doc_by_path = lexical_docs
            .iter()
            .cloned()
            .map(|result| (result.record.path.clone(), result))
            .collect::<BTreeMap<_, _>>();

        let semantic = match self.doc_semantic_hits(query, docs_limit) {
            Ok(result) if result.is_empty() => {
                warn_doc_hybrid_fallback(notes, "no doc embeddings found");
                return Ok(lexical_doc_hits(lexical_docs, scope.limit));
            }
            Ok(result) => result,
            Err(error) => {
                let reason = fallback_reason(&error);
                warn_doc_hybrid_fallback(notes, &reason);
                return Ok(lexical_doc_hits(lexical_docs, scope.limit));
            }
        };

        let records = self
            .list_docs(None, None)?
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let mut candidates = BTreeMap::<String, DocHybridCandidate>::new();
        for (path, result) in lexical_doc_by_path {
            candidates.insert(
                path,
                DocHybridCandidate {
                    hit: doc_result_to_global(result.clone(), "hybrid", None),
                    lexical_score: Some(result.score as f32),
                    semantic_score: None,
                    semantic: None,
                },
            );
        }
        for hit in semantic {
            let Some(record) = records.get(&hit.source_id) else {
                continue;
            };
            if !scope.tag_filter.is_empty() && !doc_has_all_tags(record, scope.tag_filter) {
                continue;
            }
            candidates
                .entry(hit.source_id.clone())
                .and_modify(|candidate| {
                    candidate.semantic_score = Some(hit.score);
                    candidate.semantic = Some(hit.clone());
                })
                .or_insert_with(|| DocHybridCandidate {
                    hit: GlobalSearchHit {
                        kind: "doc".to_string(),
                        source: "hybrid".to_string(),
                        id: None,
                        path: Some(record.path.clone()),
                        title: None,
                        summary: Some(record.frontmatter.summary.clone()),
                        status: Some(record.frontmatter.doc_type.as_str().to_string()),
                        best_field: None,
                        snippet: None,
                        score: None,
                        score_breakdown: None,
                        matched_by: None,
                        workspace: None,
                    },
                    lexical_score: None,
                    semantic_score: Some(hit.score),
                    semantic: Some(hit),
                });
        }

        let weight = self.docs_search_config()?.semantic_weight;
        let mut ranked = blend_doc_hybrid_candidates(candidates.into_values().collect(), weight);
        ranked.sort_by(compare_global_hits_by_score);
        ranked.truncate(scope.limit);
        Ok(ranked)
    }

    fn doc_semantic_hits(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<DocSemanticHit>, OrbitError> {
        #[cfg(test)]
        if let Some(result) = DOC_SEMANTIC_SEARCH_OVERRIDE.with(|cell| cell.borrow_mut().take()) {
            return result;
        }

        Ok(orbit_search::doc_semantic_search(
            self.stores().semantic_index().store()?,
            DocSemanticSearchParams {
                query: query.to_string(),
                limit,
                model: None,
            },
        )?
        .results)
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
