//! Workspace-local orbit-search storage.
//!
//! Module layout:
//!
//! - [`store`] — [`VectorStore`], the SQLite-backed index. Entry point.
//! - [`chunker`] — paragraph-boundary chunker for fields exceeding the
//!   model's context window.
//! - [`worker`] — optional background indexer for long-lived hosts. Short-lived
//!   CLI runtimes leave it disabled; mutations refresh through
//!   `orbit semantic index`.
//! - [`query`] — brute-force cosine, FTS5 BM25, RRF, and task-result rollup.
//! - [`task_fields`] — extracts the per-field rows that get embedded for a
//!   `Task`.
//! - [`doc_fields`] — extracts the per-field rows that get embedded for a
//!   docs corpus record.
//!
//! This file holds the small data types (`EmbeddingField`, `UpsertReport`,
//! `SemanticStats`, `SourceModelCount`) and stateless helpers
//! (`cosine_similarity`, `encode_f32_blob`, `decode_f32_blob`) shared across
//! those submodules.

pub(crate) mod chunker;
pub(crate) mod doc_fields;
pub(crate) mod query;
pub(crate) mod store;
pub(crate) mod task_fields;
pub(crate) mod worker;

pub use doc_fields::DocEmbeddingSource;
pub use store::{SOURCE_KIND_DOC, SOURCE_KIND_TASK, VectorStore};
pub use worker::EmbedWorker;

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingField {
    pub field: String,
    pub text: String,
}

impl EmbeddingField {
    pub fn new(field: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertReport {
    pub embedded_chunks: usize,
    pub skipped_fields: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceModelCount {
    pub source_kind: String,
    pub model_id: String,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticStats {
    pub counts: Vec<SourceModelCount>,
    pub stale_rows: usize,
}

pub fn encode_f32_blob(values: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(values.len() * 4);
    for value in values {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

pub fn decode_f32_blob(blob: &[u8]) -> Result<Vec<f32>, OrbitError> {
    if !blob.len().is_multiple_of(4) {
        return Err(OrbitError::Store(format!(
            "invalid embedding blob length {}; expected multiple of 4",
            blob.len()
        )));
    }
    Ok(blob
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> Result<f32, OrbitError> {
    if left.len() != right.len() {
        return Err(OrbitError::InvalidInput(format!(
            "vector length mismatch: {} != {}",
            left.len(),
            right.len()
        )));
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (l, r) in left.iter().zip(right.iter()) {
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    let denom = left_norm.sqrt() * right_norm.sqrt();
    if denom == 0.0 {
        return Ok(0.0);
    }
    Ok(dot / denom)
}

#[cfg(test)]
mod tests;
