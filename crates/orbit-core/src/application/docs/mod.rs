//! Documentation index, search, and maintenance commands.
//!
//! This module was split from a monolithic `docs.rs` (see ORB-00250).
//! Submodules own one concern each; the `impl OrbitRuntime` surface and
//! public API re-exports live here.

mod add_root;
mod artifact_ref;
mod config;
mod frontmatter;
mod migrate;
mod path_util;
mod search;
mod types;
mod walk;

#[cfg(test)]
mod tests;

use orbit_common::OrbitError;
pub use orbit_search::{DocIndexParams, DocIndexResult, SearchResult};
use orbit_search::{score_doc_record, sort_search_results};
use orbit_types::task::Task;

use crate::OrbitRuntime;

pub use config::{DocsRoot, DocsSearchConfig};
pub use types::{DocAddOutcome, DocMigrationReport, DocRecord, DocShow, DocType, TaskRelatedDoc};
pub use walk::walk_docs_roots;

// Bring helper fns into scope so the pasted impl block (lines 291-408 of original)
// continues to compile with bare calls.
use self::add_root::add_docs_root;
use self::config::{
    read_docs_roots_from_config_path, read_docs_search_config_from_config_path,
    read_task_context_docs_roots_from_config_path,
};
use self::migrate::migrate_docs;
use self::search::{doc_embedding_sources, doc_search_source, related_docs_for_context, show_doc};
use self::walk::walk_docs_with_bodies;

// The original impl block (291-408) is preserved verbatim except for path adjustments
// that are mechanical (use of super:: paths). All bodies delegate to submodules.
impl OrbitRuntime {
    pub fn docs_roots(&self) -> Result<Vec<DocsRoot>, OrbitError> {
        let config_path = self.config_path()?;
        read_docs_roots_from_config_path(&config_path)
    }

    pub fn docs_search_config(&self) -> Result<DocsSearchConfig, OrbitError> {
        let config_path = self.config_path()?;
        read_docs_search_config_from_config_path(&config_path)
    }

    pub fn list_docs(
        &self,
        doc_type: Option<DocType>,
        tag: Option<&str>,
    ) -> Result<Vec<DocRecord>, OrbitError> {
        let mut records = walk_docs_roots(&self.paths().repo_root, &self.docs_roots()?)?;
        if let Some(doc_type) = doc_type {
            records.retain(|record| record.frontmatter.doc_type == doc_type);
        }
        if let Some(tag) = tag.map(|value| value.trim().to_ascii_lowercase())
            && !tag.is_empty()
        {
            records.retain(|record| {
                record
                    .frontmatter
                    .tags
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(&tag))
            });
        }
        Ok(records)
    }

    pub fn show_doc(&self, path: &str) -> Result<DocShow, OrbitError> {
        show_doc(&self.paths().repo_root, &self.docs_roots()?, path)
    }

    pub fn search_docs(
        &self,
        query: &str,
        limit: Option<usize>,
        _include_superseded: bool,
    ) -> Result<Vec<SearchResult>, OrbitError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(OrbitError::InvalidInput(
                "docs search query must not be empty".to_string(),
            ));
        }
        let limit = limit.unwrap_or(20);
        let query_lower = query.to_ascii_lowercase();

        // One roots resolution and one walk for the whole search: every doc is
        // scored from the body that its single read already produced.
        let roots = self.docs_roots()?;
        let mut scored = Vec::new();
        for doc in walk_docs_with_bodies(&self.paths().repo_root, &roots)? {
            let source = doc_search_source(doc.record, doc.body);
            if let Some(result) = score_doc_record(source, &query_lower) {
                scored.push(SearchResult::Doc(result));
            }
        }
        sort_search_results(&mut scored);
        scored.truncate(limit);
        Ok(scored)
    }

    pub fn related_docs_for_task(
        &self,
        task: &Task,
        limit: Option<usize>,
    ) -> Result<Vec<TaskRelatedDoc>, OrbitError> {
        let config_path = self.config_path()?;
        let roots = read_task_context_docs_roots_from_config_path(&config_path)?;
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        // Tasks do not yet have a first-class `related_features` field, so the
        // agent-facing feature join uses normalized task tags as the feature
        // selectors until that storage field exists.
        related_docs_for_context(
            &self.paths().repo_root,
            &roots,
            &task.context_files,
            &task.tags,
            limit,
        )
    }

    pub fn add_docs_root(&self, path: &str) -> Result<DocAddOutcome, OrbitError> {
        add_docs_root(
            &self.paths().repo_root,
            &self.shared_root().join("config.toml"),
            path,
        )
    }

    pub fn index_docs(&self, params: DocIndexParams) -> Result<DocIndexResult, OrbitError> {
        let roots = self.docs_roots()?;
        let sources = doc_embedding_sources(&self.paths().repo_root, &roots)?;
        orbit_search::doc_index(self.stores().semantic_index().store()?, &sources, params)
    }

    pub fn migrate_docs(&self, dry_run: bool) -> Result<DocMigrationReport, OrbitError> {
        migrate_docs(&self.paths().repo_root, dry_run)
    }
}

// The three parse_*_from_config_toml live in config.rs and are re-exported above.
// No other top-level items remain in this file.
