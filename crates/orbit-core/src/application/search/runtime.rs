use orbit_common::OrbitError;

use crate::OrbitRuntime;

use super::candidates::BranchContext;
use super::filters::SearchStatusFilters;
use super::{
    GlobalSearchMode, GlobalSearchParams, GlobalSearchResponse, empty_whitespace_query_note,
    merge_round_robin,
};

impl OrbitRuntime {
    /// Rebuild the lexical task corpus, including previously unindexed tasks.
    pub fn search_reindex(&self) -> Result<orbit_search::SearchIndexStats, OrbitError> {
        let tasks = self.stores().tasks().list_tasks()?;
        self.stores().lexical_index().store()?.reindex_tasks(&tasks)
    }

    pub fn search_index_stats(&self) -> Result<orbit_search::SearchIndexStats, OrbitError> {
        self.stores().lexical_index().store()?.stats()
    }

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
    pub(in crate::application) fn workspace_search(
        &self,
        params: GlobalSearchParams,
    ) -> Result<GlobalSearchResponse, OrbitError> {
        let limit = params.normalized_limit();
        let status_filters = SearchStatusFilters::parse(&params.status)?;
        let mut notes = Vec::new();

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

        let listing = params.is_friction_listing();
        if query_owned.is_none() && !has_path && tag_filter.is_empty() && !listing {
            return Err(OrbitError::InvalidInput(
                "search requires a query, --path, or --tag; only kind friction lists without one"
                    .to_string(),
            ));
        }

        let mut branches = Vec::new();
        let mut skipped_kinds = Vec::new();
        let branch_context = BranchContext {
            params: &params,
            status_filters: &status_filters,
            query: query_owned.as_deref(),
            tag_filter: &tag_filter,
            limit,
        };

        if params.kind.includes_tasks() {
            branches.push(self.task_branch(branch_context)?);
        }

        if params.kind.includes_frictions() {
            if has_path {
                notes.push(
                    "friction branch skipped: --path is set; frictions are not path-filtered"
                        .to_string(),
                );
                skipped_kinds.push("friction".to_string());
            } else {
                let (hits, truncated) = self.friction_branch(
                    &params,
                    &status_filters,
                    query_owned.as_deref(),
                    &tag_filter,
                    limit,
                )?;
                if listing && truncated {
                    notes.push(format!(
                        "friction listing truncated at {limit} records; narrow it with a `friction:` status or a tag"
                    ));
                }
                branches.push(hits);
            }
        }

        let results = merge_round_robin(branches, limit);
        if results.is_empty()
            && let Some(query) = query_owned.as_deref()
            && let Some(note) = empty_whitespace_query_note(query)
        {
            notes.push(note);
        }
        Ok(GlobalSearchResponse {
            mode: GlobalSearchMode::Lexical,
            kind: params.kind,
            results,
            notes,
            skipped_kinds,
            workspaces: Vec::new(),
        })
    }
}
