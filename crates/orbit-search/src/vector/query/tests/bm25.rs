//! Unit tests for `bm25` — sibling layout under vector/query/tests/.

use rusqlite::Connection;

use super::super::bm25::{bm25_top_k, fts_terms_query, snippet_for_hit};

use crate::NoopEmbedder;
use crate::vector::store::schema::SEMANTIC_INDEX_LAYOUT_INCOMPATIBLE;
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

    let hits = bm25_top_k(&store, "neutrino", Some("task"), None, 3).unwrap();

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

    let task_hits = bm25_top_k(&store, "neutrino", Some("task"), None, 10).unwrap();
    let all_hits = bm25_top_k(&store, "neutrino", None, None, 10).unwrap();

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

    let hits = bm25_top_k(&store, "neutrino decay", Some("task"), None, 5).unwrap();
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

/// Opening a pre-[ORB-11695] inline `corpus_fts` through the current runtime
/// migrates in place; BM25 and snippets then read `chunks`, and the migrated
/// rows stay put. This is not a dual-read shim for older binaries.
#[test]
fn bm25_and_snippets_survive_inline_to_external_content_migration() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("semantic.db");
    {
        let conn = Connection::open(&path).expect("seed db");
        conn.execute_batch(
            r#"
                CREATE VIRTUAL TABLE corpus_fts USING fts5(
                    source_kind UNINDEXED,
                    source_id UNINDEXED,
                    field UNINDEXED,
                    content,
                    tokenize = 'porter unicode61 remove_diacritics 2'
                );
                INSERT INTO corpus_fts(source_kind, source_id, field, content)
                VALUES
                    ('task', 'T1', 'purpose', 'gamma neutrino'),
                    ('doc', 'docs/a.md', 'body', 'unrelated');
            "#,
        )
        .expect("seed inline corpus");
    }

    let store = VectorStore::open(&path).expect("open migrates layout");
    let hits = bm25_top_k(&store, "neutrino", Some("task"), None, 5).expect("current bm25");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].source_kind, "task");
    assert_eq!(hits[0].source_id, "T1");
    assert_eq!(hits[0].field, "purpose");

    let snippet = snippet_for_hit(&store, "task", "T1", "purpose", Some(0), None)
        .expect("snippet")
        .expect("migrated chunk text");
    assert_eq!(snippet, "gamma neutrino");

    let conn = store.connection();
    let conn = conn.lock().expect("lock");
    let leftover_inline: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('corpus_fts') WHERE name = 'source_kind')",
            [],
            |row| row.get(0),
        )
        .expect("pragma");
    assert_eq!(
        leftover_inline, 0,
        "migration must not restore inline columns"
    );
    let chunks: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
        .expect("chunk count");
    assert_eq!(chunks, 2);

    let mismatch =
        crate::vector::store::schema::evaluate_legacy_inline_reader(&conn, "neutrino", "task")
            .expect_err("old inline reader against migrated store");
    let message = mismatch.to_string();
    assert!(
        message.contains(SEMANTIC_INDEX_LAYOUT_INCOMPATIBLE),
        "{message}"
    );
    assert!(!message.to_ascii_lowercase().contains("no such column"));
}
