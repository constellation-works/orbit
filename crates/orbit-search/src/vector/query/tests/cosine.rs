//! Unit tests for `cosine` — sibling layout under vector/query/tests/.

use super::super::cosine::cosine_top_k;

use crate::vector::{EmbeddingField, VectorStore};
use crate::{Embedder, NoopEmbedder};

#[test]
fn cosine_top_k_returns_expected_ordering_with_noop_vectors() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "T1",
            &[EmbeddingField::new("purpose", "alpha")],
            &embedder,
            false,
        )
        .unwrap();
    store
        .upsert_embeddings(
            "task",
            "T2",
            &[EmbeddingField::new("purpose", "beta")],
            &embedder,
            false,
        )
        .unwrap();
    store
        .upsert_embeddings(
            "task",
            "T3",
            &[EmbeddingField::new("purpose", "gamma")],
            &embedder,
            false,
        )
        .unwrap();

    let query = embedder.embed(&["beta"]).unwrap().remove(0);
    let hits = cosine_top_k(&store, &query, embedder.model_id(), 3, Some("task"), None).unwrap();

    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].source_id, "T2");
    assert_eq!(hits[0].rank, 1);
    assert!((hits[0].score - 1.0).abs() < 0.0001);
}
