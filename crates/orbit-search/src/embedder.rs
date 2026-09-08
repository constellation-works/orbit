//! `Embedder` trait + the model catalog.
//!
//! `Embedder` is the small abstraction the rest of the crate (and consumers)
//! lean on: produce embeddings for `&[&str]`, count tokens for chunking,
//! and self-describe via `model_id` / `dim` / `max_input_tokens`. The catalog
//! pins the three fastembed-rs models the install command knows how to fetch.

// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use orbit_common::OrbitError;

pub const DEFAULT_MODEL: &str = "bge-small";

pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
    fn max_input_tokens(&self) -> usize;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError>;
    fn token_count(&self, text: &str) -> Result<usize, OrbitError>;

    /// Byte positions immediately after each model token in `text`.
    ///
    /// Chunking uses these positions to select likely boundaries without
    /// treating independent token counts as additive. Callers still validate
    /// every emitted chunk with `token_count` because model tokenization may
    /// depend on the text surrounding a boundary.
    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub alias: &'static str,
    pub fastembed_name: &'static str,
    pub dim: usize,
    pub max_input_tokens: usize,
}

impl ModelSpec {
    pub fn parse(value: &str) -> Result<Self, OrbitError> {
        let normalized = value.trim().to_ascii_lowercase();
        supported_models()
            .iter()
            .copied()
            .find(|model| {
                model.alias == normalized || model.fastembed_name.eq_ignore_ascii_case(value.trim())
            })
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "unsupported semantic model `{value}`; expected one of: {}",
                    supported_models()
                        .iter()
                        .map(|model| model.alias)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }
}

pub fn default_model() -> ModelSpec {
    ModelSpec::parse(DEFAULT_MODEL).expect("default semantic model is supported")
}

pub fn supported_models() -> &'static [ModelSpec] {
    &[
        ModelSpec {
            alias: "bge-small",
            fastembed_name: "BGESmallENV15",
            dim: 384,
            max_input_tokens: 512,
        },
        ModelSpec {
            alias: "minilm-l6",
            fastembed_name: "AllMiniLML6V2",
            dim: 384,
            max_input_tokens: 512,
        },
        ModelSpec {
            alias: "nomic-v1.5",
            fastembed_name: "NomicEmbedTextV15",
            dim: 768,
            max_input_tokens: 8192,
        },
    ]
}
