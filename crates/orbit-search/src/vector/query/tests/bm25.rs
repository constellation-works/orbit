//! Unit tests for `bm25` — sibling layout under vector/query/tests/.

use super::super::bm25::{bm25_top_k, fts_terms_query, snippet_for_hit};

use crate::NoopEmbedder;
use crate::vector::{EmbeddingField, VectorStore};

#[test]
fn bm25_top_k_ranks_lexical_matches() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    for (id, text) in [
        ("T1", "alpha beta"),
        ("T2", "neutrino unique token"),
        ("T3", "gamma delta"),
    ] {
        store
            .upsert_embeddings(
                "task",
                id,
                &[EmbeddingField::new("purpose", text)],
                &embedder,
                false,
            )
            .unwrap();
    }

    let hits = bm25_top_k(&store, "neutrino", Some("task"), 3).unwrap();

    assert_eq!(hits[0].source_id, "T2");
    assert_eq!(hits[0].field, "purpose");
    assert_eq!(hits[0].rank, 1);
}

#[test]
fn bm25_top_k_filters_by_source_kind() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "T1",
            &[EmbeddingField::new("purpose", "neutrino task")],
            &embedder,
            false,
        )
        .unwrap();
    store
        .upsert_embeddings(
            "doc",
            "D1",
            &[EmbeddingField::new("summary", "neutrino doc")],
            &embedder,
            false,
        )
        .unwrap();

    let task_hits = bm25_top_k(&store, "neutrino", Some("task"), 10).unwrap();
    let all_hits = bm25_top_k(&store, "neutrino", None, 10).unwrap();

    assert_eq!(task_hits.len(), 1);
    assert_eq!(task_hits[0].source_kind, "task");
    assert_eq!(all_hits.len(), 2);
}

#[test]
fn bm25_terms_are_quoted_individually_not_as_one_phrase() {
    assert_eq!(
        fts_terms_query("foo \"bar\" baz"),
        "\"foo\" \"\"\"bar\"\"\" \"baz\""
    );
    assert_eq!(
        fts_terms_query("  neutrino   decay "),
        "\"neutrino\" \"decay\""
    );
    assert_eq!(fts_terms_query("ORB-11136"), "\"ORB-11136\"");
}

/// The words of a query need not be adjacent in the chunk: `neutrino decay`
/// must still find a chunk that says `neutrino ... decay`.
#[test]
fn bm25_multi_word_query_matches_non_adjacent_terms() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    for (id, text) in [
        ("T1", "flaky neutrino harness on the decay path"),
        ("T2", "neutrino only"),
        ("T3", "unrelated gamma delta"),
    ] {
        store
            .upsert_embeddings(
                "task",
                id,
                &[EmbeddingField::new("purpose", text)],
                &embedder,
                false,
            )
            .unwrap();
    }

    let hits = bm25_top_k(&store, "neutrino decay", Some("task"), 5).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].source_id, "T1");
}

#[test]
fn snippet_lookup_preserves_chunk_order() {
    let store = VectorStore::open_in_memory().unwrap();
    let conn = store.connection();
    let conn = conn.lock().unwrap();
    conn.execute(
        "INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content) VALUES (?1, ?2, ?3, ?4, ?5)",
        ("task", "T1", "purpose", 0, "first chunk"),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content) VALUES (?1, ?2, ?3, ?4, ?5)",
        ("task", "T1", "purpose", 1, "second chunk"),
    )
    .unwrap();
    drop(conn);

    let snippet = snippet_for_hit(&store, "task", "T1", "purpose", Some(1), None).unwrap();

    assert_eq!(snippet.as_deref(), Some("second chunk"));
}
