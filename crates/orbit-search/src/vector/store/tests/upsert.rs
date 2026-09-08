//! Unit tests for `upsert` — sibling layout under store/tests/.

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use orbit_common::OrbitError;

use crate::vector::query::cosine_top_k;
use crate::vector::{EmbeddingField, VectorStore};
use crate::{Embedder, NoopEmbedder};

const UNBLOCKED_WAIT: Duration = Duration::from_secs(5);

#[test]
fn upsert_embeddings_skips_unchanged_content_hashes() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    let fields = vec![EmbeddingField::new("purpose", "same content")];

    let first = store
        .upsert_embeddings("task", "T1", &fields, &embedder, false)
        .unwrap();
    let second = store
        .upsert_embeddings("task", "T1", &fields, &embedder, false)
        .unwrap();

    assert_eq!(first.embedded_chunks, 1);
    assert_eq!(second.embedded_chunks, 0);
    assert_eq!(second.skipped_fields, 1);
}

#[test]
fn upsert_drops_fields_absent_from_the_complete_set_without_reembedding() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "T1",
            &[
                EmbeddingField::new("title", "keep me"),
                EmbeddingField::new("extra", "drop me"),
            ],
            &embedder,
            false,
        )
        .unwrap();

    let report = store
        .upsert_embeddings(
            "task",
            "T1",
            &[EmbeddingField::new("title", "keep me")],
            &embedder,
            false,
        )
        .unwrap();

    assert_eq!(report.embedded_chunks, 0);
    assert_eq!(report.skipped_fields, 1);
    assert_eq!(
        field_contents(&store, "T1"),
        BTreeMap::from([("title".to_string(), "keep me".to_string())])
    );
}

#[test]
fn failed_embedding_leaves_previously_committed_source_including_fields_scheduled_for_removal() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "T1",
            &[
                EmbeddingField::new("title", "original title"),
                EmbeddingField::new("extra", "must survive"),
            ],
            &embedder,
            false,
        )
        .unwrap();

    let error = store
        .upsert_embeddings(
            "task",
            "T1",
            &[EmbeddingField::new("title", "replacement that will fail")],
            &FailingEmbedder {
                inner: NoopEmbedder::small(),
            },
            false,
        )
        .expect_err("embed failure should abort before any write");

    assert!(
        error.to_string().contains("forced embed failure"),
        "unexpected error: {error}"
    );
    assert_eq!(
        field_contents(&store, "T1"),
        BTreeMap::from([
            ("title".to_string(), "original title".to_string()),
            ("extra".to_string(), "must survive".to_string()),
        ])
    );
}

#[test]
fn inference_does_not_hold_store_mutex_or_sqlite_write_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("semantic.db");
    let store = VectorStore::open(&path).unwrap();
    let noop = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "VISIBLE",
            &[EmbeddingField::new("purpose", "beta")],
            &noop,
            false,
        )
        .unwrap();
    store
        .upsert_embeddings(
            "task",
            "SLOW",
            &[EmbeddingField::new("purpose", "old slow")],
            &noop,
            false,
        )
        .unwrap();

    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let embedder = BarrierEmbedder {
        inner: noop.clone(),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    };

    let store_for_upsert = store.clone();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = store_for_upsert.upsert_embeddings(
            "task",
            "SLOW",
            &[EmbeddingField::new("purpose", "new slow")],
            &embedder,
            false,
        );
        done_tx
            .send(result.map(|report| report.embedded_chunks))
            .unwrap();
    });
    wait_until_inference_started(&started);

    let query = noop.embed(&["beta"]).unwrap().remove(0);
    let store_for_query = store.clone();
    let hits = run_unblocked("query on shared store", move || {
        cosine_top_k(
            &store_for_query,
            &query,
            noop.model_id(),
            3,
            Some("task"),
            None,
        )
    });
    assert!(
        hits.iter().any(|hit| hit.source_id == "VISIBLE"),
        "shared-store query should see the already-committed source: {hits:?}"
    );

    let path_for_second = path.clone();
    run_unblocked("unrelated write on second connection", move || {
        let second = VectorStore::open(&path_for_second).unwrap();
        second.upsert_embeddings(
            "task",
            "OTHER",
            &[EmbeddingField::new("purpose", "unrelated")],
            &NoopEmbedder::small(),
            false,
        )
    });

    release.wait();
    let embedded = done_rx
        .recv_timeout(UNBLOCKED_WAIT)
        .expect("paused upsert should finish after release")
        .expect("paused upsert should commit");
    assert_eq!(embedded, 1);
    assert_eq!(
        field_contents(&store, "OTHER"),
        BTreeMap::from([("purpose".to_string(), "unrelated".to_string())])
    );
}

#[test]
fn concurrent_same_source_replacement_keeps_the_first_committed_complete_set() {
    let store = VectorStore::open_in_memory().unwrap();
    let noop = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "S1",
            &[
                EmbeddingField::new("title", "keep"),
                EmbeddingField::new("body", "old"),
                EmbeddingField::new("extra", "remove-me"),
            ],
            &noop,
            false,
        )
        .unwrap();

    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let embedder = BarrierEmbedder {
        inner: noop.clone(),
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    };

    let store_for_stale = store.clone();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = store_for_stale.upsert_embeddings(
            "task",
            "S1",
            &[
                EmbeddingField::new("title", "keep"),
                EmbeddingField::new("body", "stale replacement"),
            ],
            &embedder,
            false,
        );
        done_tx
            .send(result.err().map(|error| error.to_string()))
            .unwrap();
    });
    wait_until_inference_started(&started);

    store
        .upsert_embeddings(
            "task",
            "S1",
            &[
                EmbeddingField::new("title", "newer"),
                EmbeddingField::new("body", "old"),
            ],
            &noop,
            false,
        )
        .unwrap();

    release.wait();
    let error = done_rx
        .recv_timeout(UNBLOCKED_WAIT)
        .expect("stale upsert should finish after release")
        .expect("stale upsert should abort");
    assert!(
        error.contains("changed during embedding"),
        "unexpected error: {error}"
    );
    assert_eq!(
        field_contents(&store, "S1"),
        BTreeMap::from([
            ("title".to_string(), "newer".to_string()),
            ("body".to_string(), "old".to_string()),
        ])
    );
}

#[test]
fn concurrent_same_source_delete_is_not_resurrected_by_in_flight_upsert() {
    let store = VectorStore::open_in_memory().unwrap();
    let noop = NoopEmbedder::small();
    store
        .upsert_embeddings(
            "task",
            "S1",
            &[EmbeddingField::new("purpose", "doomed")],
            &noop,
            false,
        )
        .unwrap();

    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let embedder = BarrierEmbedder {
        inner: noop,
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    };

    let store_for_stale = store.clone();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = store_for_stale.upsert_embeddings(
            "task",
            "S1",
            &[EmbeddingField::new("purpose", "should not resurrect")],
            &embedder,
            false,
        );
        done_tx
            .send(result.err().map(|error| error.to_string()))
            .unwrap();
    });
    wait_until_inference_started(&started);

    store.delete_source("task", "S1").unwrap();

    release.wait();
    let error = done_rx
        .recv_timeout(UNBLOCKED_WAIT)
        .expect("stale upsert should finish after release")
        .expect("stale upsert should abort");
    assert!(
        error.contains("changed during embedding"),
        "unexpected error: {error}"
    );
    assert!(field_contents(&store, "S1").is_empty());
}

fn field_contents(store: &VectorStore, source_id: &str) -> BTreeMap<String, String> {
    let conn = store.connection();
    let conn = conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            r#"
                SELECT field, content
                FROM chunks
                WHERE source_kind = 'task' AND source_id = ?1
                ORDER BY field, chunk_idx
            "#,
        )
        .unwrap();
    let rows = stmt
        .query_map([source_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap();
    let mut fields = BTreeMap::new();
    for row in rows {
        let (field, content) = row.unwrap();
        fields.insert(field, content);
    }
    fields
}

fn wait_until_inference_started(started: &Arc<Barrier>) {
    let started = Arc::clone(started);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        started.wait();
        tx.send(()).unwrap();
    });
    rx.recv_timeout(UNBLOCKED_WAIT)
        .expect("inference should start without holding the store mutex");
}

fn run_unblocked<T, F>(label: &str, work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, OrbitError> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(work()).unwrap();
    });
    rx.recv_timeout(UNBLOCKED_WAIT)
        .unwrap_or_else(|_| panic!("{label} blocked while inference was paused"))
        .unwrap_or_else(|error| panic!("{label} failed: {error}"))
}

struct BarrierEmbedder {
    inner: NoopEmbedder,
    started: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl Embedder for BarrierEmbedder {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn max_input_tokens(&self) -> usize {
        self.inner.max_input_tokens()
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        self.started.wait();
        self.release.wait();
        self.inner.embed(texts)
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        self.inner.token_count(text)
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        self.inner.token_boundaries(text)
    }
}

struct FailingEmbedder {
    inner: NoopEmbedder,
}

impl Embedder for FailingEmbedder {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn max_input_tokens(&self) -> usize {
        self.inner.max_input_tokens()
    }

    fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        Err(OrbitError::Execution("forced embed failure".into()))
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        self.inner.token_count(text)
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        self.inner.token_boundaries(text)
    }
}
