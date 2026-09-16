//! Docs-corpus indexing entry points, and the read-back of what they stored.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use rusqlite::params;

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

/// The frontmatter one indexed doc can be described with without opening
/// its file: the `title` and `tags` fields [`VectorStore::index_doc`] stored
/// for it. The doc type is not indexed, so a hit completed from here carries
/// none [DANI-10369].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedDocFields {
    pub title: String,
    pub tags: Vec<String>,
}

impl VectorStore {
    /// The stored `title`/`tags` fields of each named doc, keyed by path.
    ///
    /// A path with no doc rows is absent from the map. Both fields are read
    /// from their first chunk: a summary line or tag list never reaches the
    /// chunker's split size in practice.
    pub fn indexed_doc_fields(
        &self,
        source_ids: &[&str],
    ) -> Result<BTreeMap<String, IndexedDocFields>, OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        let mut stmt = conn
            .prepare_cached(
                r#"
                    SELECT field, content
                    FROM chunks
                    WHERE source_kind = ?1 AND source_id = ?2
                        AND field IN ('title', 'tags') AND chunk_idx = 0
                "#,
            )
            .map_err(|error| super::schema::translate_corpus_fts_sql_error(&conn, error))?;
        let mut out = BTreeMap::new();
        for source_id in source_ids {
            let rows = stmt
                .query_map(params![SOURCE_KIND_DOC, source_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            for row in rows {
                let (field, content) = row.map_err(|error| OrbitError::Store(error.to_string()))?;
                let fields: &mut IndexedDocFields =
                    out.entry((*source_id).to_string()).or_default();
                match field.as_str() {
                    "title" => fields.title = content,
                    "tags" => {
                        fields.tags = content
                            .lines()
                            .map(str::trim)
                            .filter(|tag| !tag.is_empty())
                            .map(str::to_string)
                            .collect();
                    }
                    _ => {}
                }
            }
        }
        Ok(out)
    }

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
