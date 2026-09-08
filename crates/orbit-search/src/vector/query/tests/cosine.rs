//! Unit tests for `cosine` — sibling layout under vector/query/tests/.

use rusqlite::params;

use super::super::cosine::cosine_top_k;

use crate::vector::{EmbeddingField, VectorStore, cosine_similarity, encode_f32_blob};
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

#[test]
fn cosine_top_k_matches_exhaustive_reference_for_limits_ties_and_filters() {
    let store = VectorStore::open_in_memory().unwrap();
    let query = [1.0, 0.0];
    let rows = [
        ("doc", "D2", "summary", 2, "model-a", [1.0, 0.0]),
        ("task", "T2", "purpose", 1, "model-a", [1.0, 0.0]),
        ("task", "T1", "zeta", 4, "model-a", [1.0, 0.0]),
        ("task", "T1", "alpha", 3, "model-a", [1.0, 0.0]),
        ("task", "T0", "purpose", 0, "model-a", [0.0, 1.0]),
        ("task", "T3", "purpose", 5, "model-a", [0.6, 0.8]),
        ("task", "B1", "purpose", 0, "model-b", [1.0, 0.0]),
    ];
    for (source_kind, source_id, field, chunk_idx, model_id, embedding) in rows {
        insert_embedding(
            &store,
            source_kind,
            source_id,
            field,
            chunk_idx,
            model_id,
            &embedding,
        );
    }

    let mut reference = rows
        .iter()
        .filter(|(_, _, _, _, model_id, _)| *model_id == "model-a")
        .map(|(source_kind, source_id, field, chunk_idx, _, embedding)| {
            let score = cosine_similarity(&query, embedding).unwrap();
            super::super::cosine::CosineHit {
                source_kind: (*source_kind).to_string(),
                source_id: (*source_id).to_string(),
                field: (*field).to_string(),
                chunk_idx: *chunk_idx,
                score,
                rank: 0,
            }
        })
        .collect::<Vec<_>>();
    reference.sort_by(super::super::cosine::compare_cosine_hits);

    for limit in [1, 2, 3, 4, 6, 10] {
        let hits = cosine_top_k(&store, &query, "model-a", limit, None, None).unwrap();
        let expected = ranked(&reference[..limit.min(reference.len())]);
        assert_eq!(hits, expected);
    }

    let task_reference = reference
        .iter()
        .filter(|hit| hit.source_kind == "task")
        .cloned()
        .collect::<Vec<_>>();
    let task_hits = cosine_top_k(&store, &query, "model-a", 10, Some("task"), None).unwrap();
    assert_eq!(task_hits, ranked(&task_reference));

    let purpose_reference = task_reference
        .iter()
        .filter(|hit| hit.field == "purpose")
        .cloned()
        .collect::<Vec<_>>();
    let purpose_hits =
        cosine_top_k(&store, &query, "model-a", 10, Some("task"), Some("purpose")).unwrap();
    assert_eq!(purpose_hits, ranked(&purpose_reference));

    assert!(
        cosine_top_k(&store, &[], "model-a", 10, None, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        cosine_top_k(&store, &query, "model-a", 0, None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        cosine_top_k(&store, &query, "model-b", 10, None, None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cosine_top_k_keeps_large_corpus_results_bounded_by_limit() {
    let store = VectorStore::open_in_memory().unwrap();
    let connection = store.connection();
    let mut connection = connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..4096 {
        let source_id = format!("source-{index:04}");
        transaction
            .execute(
                "INSERT INTO embeddings(source_kind, source_id, field, chunk_idx, content_hash, model_id, dim, embedding, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "task",
                    source_id,
                    "purpose",
                    0_i64,
                    "test-hash",
                    "model-a",
                    2_i64,
                    encode_f32_blob(&[1.0, 0.0]),
                    "2026-01-01T00:00:00Z",
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);

    let limit = 7;
    let hits = cosine_top_k(&store, &[1.0, 0.0], "model-a", limit, None, None).unwrap();

    assert_eq!(hits.len(), limit);
    assert!(hits.capacity() <= limit);
    assert_eq!(hits[0].source_id, "source-0000");
    assert_eq!(hits[limit - 1].source_id, "source-0006");
}

fn ranked(hits: &[super::super::cosine::CosineHit]) -> Vec<super::super::cosine::CosineHit> {
    hits.iter()
        .cloned()
        .enumerate()
        .map(|(index, mut hit)| {
            hit.rank = index + 1;
            hit
        })
        .collect()
}

fn insert_embedding(
    store: &VectorStore,
    source_kind: &str,
    source_id: &str,
    field: &str,
    chunk_idx: usize,
    model_id: &str,
    embedding: &[f32],
) {
    let connection = store.connection();
    let connection = connection.lock().unwrap();
    connection
        .execute(
            "INSERT INTO embeddings(source_kind, source_id, field, chunk_idx, content_hash, model_id, dim, embedding, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                source_kind,
                source_id,
                field,
                chunk_idx as i64,
                "test-hash",
                model_id,
                embedding.len() as i64,
                encode_f32_blob(embedding),
                "2026-01-01T00:00:00Z",
            ],
        )
        .unwrap();
}
