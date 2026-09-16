//! Docs-corpus indexing entry points.

use std::collections::BTreeSet;

use orbit_common::OrbitError;

use super::{SOURCE_KIND_DOC, VectorStore};
use crate::Embedder;
use crate::vector::UpsertReport;
use crate::vector::doc_fields::{DocEmbeddingSource, doc_embedding_fields};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocReindexReport {
    pub upsert: UpsertReport,
    pub indexed_sources: usize,
    pub stale_sources: Vec<String>,
}

impl VectorStore {
    pub fn index_doc(
        &self,
        doc: &DocEmbeddingSource,
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<UpsertReport, OrbitError> {
        self.upsert_embeddings(
            SOURCE_KIND_DOC,
            &doc.path,
            &doc_embedding_fields(doc),
            embedder,
            force,
        )
    }

    pub fn reindex_docs(
        &self,
        docs: &[DocEmbeddingSource],
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<DocReindexReport, OrbitError> {
        let live = docs
            .iter()
            .map(|doc| doc.path.clone())
            .collect::<BTreeSet<_>>();
        let stale_sources = self
            .source_ids(SOURCE_KIND_DOC)?
            .difference(&live)
            .cloned()
            .collect::<Vec<_>>();
        let mut sources = docs
            .iter()
            .map(|doc| (doc.path.clone(), doc_embedding_fields(doc)))
            .collect::<Vec<_>>();
        sources.extend(
            stale_sources
                .iter()
                .cloned()
                .map(|source_id| (source_id, Vec::new())),
        );
        let source_refs = sources
            .iter()
            .map(|(source_id, fields)| (source_id.as_str(), fields.as_slice()))
            .collect::<Vec<_>>();
        let upsert =
            self.upsert_embedding_sources(SOURCE_KIND_DOC, &source_refs, embedder, force)?;

        Ok(DocReindexReport {
            upsert,
            indexed_sources: live.len(),
            stale_sources,
        })
    }
}
