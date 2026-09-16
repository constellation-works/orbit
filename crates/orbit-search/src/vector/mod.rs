//! Workspace-local orbit-search storage.
//!
//! Module layout:
//!
//! - [`store`] — [`VectorStore`], the SQLite-backed index. Entry point.
//! - [`index`] — [`SemanticIndex`], the optional handle a runtime holds: an
//!   open store, or the explained absence semantic callers are told about.
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
//! (`cosine_similarity_blob`, `dot_product_blob`, `encode_f32_blob`,
//! `normalize_f32`) shared across those submodules.

pub(crate) mod chunker;
pub(crate) mod doc_fields;
pub(crate) mod index;
pub(crate) mod query;
pub(crate) mod store;
pub(crate) mod task_fields;
pub(crate) mod worker;

pub use doc_fields::DocEmbeddingSource;
pub use index::SemanticIndex;
pub use query::{Bm25Hit, bm25_top_k};
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

#[cfg(test)]
pub(crate) fn decode_f32_blob(blob: &[u8]) -> Result<Vec<f32>, OrbitError> {
    Ok(le_f32_chunks(blob)?
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

/// View a little-endian f32 blob as 4-byte chunks without allocating.
pub(crate) fn le_f32_chunks(blob: &[u8]) -> Result<&[[u8; 4]], OrbitError> {
    if !blob.len().is_multiple_of(4) {
        return Err(OrbitError::Store(format!(
            "invalid embedding blob length {}; expected multiple of 4",
            blob.len()
        )));
    }
    let (chunks, remainder) = blob.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    Ok(chunks)
}

pub(crate) fn l2_norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// L2-normalise `values`. A zero vector is returned unchanged.
pub(crate) fn normalize_f32(values: &[f32]) -> Vec<f32> {
    let norm = l2_norm(values);
    if norm == 0.0 {
        values.to_vec()
    } else {
        values.iter().map(|value| value / norm).collect()
    }
}

#[cfg(test)]
pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> Result<f32, OrbitError> {
    if left.len() != right.len() {
        return Err(length_mismatch(left.len(), right.len()));
    }
    finish_cosine(dot_and_norm2(
        left.iter().copied().zip(right.iter().copied()),
    ))
}

/// Cosine similarity between a query slice and an LE f32 blob, with no heap
/// allocation. Used for legacy (unnormalised) stored rows.
pub(crate) fn cosine_similarity_blob(query: &[f32], blob: &[u8]) -> Result<f32, OrbitError> {
    let chunks = le_f32_chunks(blob)?;
    if query.len() != chunks.len() {
        return Err(length_mismatch(query.len(), chunks.len()));
    }
    finish_cosine(dot_and_norm2(
        query
            .iter()
            .copied()
            .zip(chunks.iter().map(|chunk| f32::from_le_bytes(*chunk))),
    ))
}

/// Dot product between a query slice and an LE f32 blob, with no heap
/// allocation. Equal to cosine when both sides are unit-length.
pub(crate) fn dot_product_blob(query: &[f32], blob: &[u8]) -> Result<f32, OrbitError> {
    let chunks = le_f32_chunks(blob)?;
    if query.len() != chunks.len() {
        return Err(length_mismatch(query.len(), chunks.len()));
    }
    Ok(query
        .iter()
        .zip(chunks)
        .map(|(left, chunk)| left * f32::from_le_bytes(*chunk))
        .sum())
}

fn length_mismatch(left: usize, right: usize) -> OrbitError {
    OrbitError::InvalidInput(format!("vector length mismatch: {left} != {right}"))
}

fn dot_and_norm2(pairs: impl Iterator<Item = (f32, f32)>) -> (f32, f32, f32) {
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (left, right) in pairs {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    (dot, left_norm, right_norm)
}

fn finish_cosine((dot, left_norm, right_norm): (f32, f32, f32)) -> Result<f32, OrbitError> {
    let denom = left_norm.sqrt() * right_norm.sqrt();
    if denom == 0.0 {
        return Ok(0.0);
    }
    Ok(dot / denom)
}

#[cfg(test)]
mod tests;
