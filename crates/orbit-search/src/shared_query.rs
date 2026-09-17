//! One query-side embedder shared by every reader of a single query.
//!
//! A fan-out that asks N indexes the same question otherwise pays the query
//! side N times: N companion spawns, each loading the model, and N identical
//! embeddings of one string. Both costs collapse here — the companion is
//! borrowed from the host's [`EmbedderPool`], which already holds a warm one
//! on a long-lived host and spawns at most one otherwise, and a batch
//! identical to the previous one is answered from the memo [DANI-10365].
//! Because the companion is the pool's, so is its stderr policy: a federated
//! query is exactly as loud as a single-workspace one on the same host.
//!
//! The memo is deliberately one entry deep. A query embeds the same text for
//! every reader and every branch, so a single slot is the whole win; a growing
//! map would only retain vectors nothing asks for again.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use orbit_common::OrbitError;

use crate::embedder::Embedder;
use crate::pool::EmbedderPool;

/// One batch and the vectors it produced.
#[derive(Debug)]
struct Memo {
    texts: Vec<String>,
    vectors: Vec<Vec<f32>>,
}

impl Memo {
    fn answers(&self, texts: &[&str]) -> bool {
        self.texts.len() == texts.len()
            && self
                .texts
                .iter()
                .zip(texts)
                .all(|(memoized, text)| memoized.as_str() == *text)
    }
}

/// An [`Embedder`] that answers a repeated batch without re-embedding it.
pub struct SharedQueryEmbedder {
    inner: Arc<dyn Embedder>,
    memo: Mutex<Option<Memo>>,
}

impl SharedQueryEmbedder {
    /// Wrap an embedder so repeated identical batches are embedded once.
    pub fn new(inner: Arc<dyn Embedder>) -> Self {
        Self {
            inner,
            memo: Mutex::new(None),
        }
    }

    /// Borrow the pool's companion for `model` and share it across a fan-out.
    ///
    /// `model` is the canonical alias [`crate::query_model_id`] resolves — the
    /// pool's cache key — so a warm host hands back its live companion and a
    /// cold one spawns exactly one, under the pool's stderr policy.
    pub fn from_pool(embedders: &EmbedderPool, model: &str) -> Result<Self, OrbitError> {
        Ok(Self::new(embedders.embedder(model)?))
    }
}

impl fmt::Debug for SharedQueryEmbedder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedQueryEmbedder")
            .field("model_id", &self.inner.model_id())
            .finish_non_exhaustive()
    }
}

impl Embedder for SharedQueryEmbedder {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn max_input_tokens(&self) -> usize {
        self.inner.max_input_tokens()
    }

    /// Embed `texts`, or return the previous batch's vectors when the batch is
    /// unchanged.
    ///
    /// The memo lock is held across the inner call on purpose: readers racing
    /// on the same query wait for one embedding rather than each starting its
    /// own. A failed embed is not memoized, so one reader's transient
    /// companion failure is not inherited by the next.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(memoized) = memo.as_ref()
            && memoized.answers(texts)
        {
            return Ok(memoized.vectors.clone());
        }
        let vectors = self.inner.embed(texts)?;
        *memo = Some(Memo {
            texts: texts.iter().map(|text| (*text).to_string()).collect(),
            vectors: vectors.clone(),
        });
        Ok(vectors)
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        self.inner.token_count(text)
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        self.inner.token_boundaries(text)
    }
}
