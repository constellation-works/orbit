//! Unit tests for `queries` — sibling layout under store/tests/.

use crate::NoopEmbedder;
use crate::vector::EmbeddingField;
use crate::vector::VectorStore;

#[test]
fn delete_source_cascades_vector_and_fts_rows() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "T1",
            &[EmbeddingField::new("purpose", "delete me")],
            &embedder,
            false,
        )
        .unwrap();

    store.delete_source("task", "T1").unwrap();

    let conn = store.connection();
    let conn = conn.lock().unwrap();
    let embeddings: i64 = conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))
        .unwrap();
    let chunks: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    let fts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM corpus_fts WHERE corpus_fts MATCH 'delete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(embeddings, 0);
    assert_eq!(chunks, 0);
    assert_eq!(fts, 0);
}
