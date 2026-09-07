//! Unit tests for `docs` — sibling layout under store/tests/.

use crate::NoopEmbedder;
use crate::vector::{DocEmbeddingSource, SOURCE_KIND_DOC, VectorStore};

fn doc(path: &str, title: &str, body: &str) -> DocEmbeddingSource {
    DocEmbeddingSource {
        path: path.to_string(),
        title: title.to_string(),
        tags: vec!["docs".to_string()],
        body: body.to_string(),
    }
}

#[test]
fn noop_doc_indexing_populates_doc_rows() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();

    let report = store
        .index_doc(
            &doc("docs/example.md", "Example", "semantic docs body"),
            &embedder,
            false,
        )
        .unwrap();
    let stats = store.stats(&[]).unwrap();

    assert!(report.embedded_chunks >= 3);
    assert_eq!(stats.counts[0].source_kind, "doc");
    assert_eq!(stats.counts[0].model_id, "noop");
}

#[test]
fn reindex_docs_removes_stale_sources() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .index_doc(&doc("docs/old.md", "Old", "old body"), &embedder, false)
        .unwrap();

    let report = store
        .reindex_docs(&[doc("docs/new.md", "New", "new body")], &embedder, false)
        .unwrap();
    let source_ids = store.source_ids(SOURCE_KIND_DOC).unwrap();

    assert_eq!(report.stale_sources, vec!["docs/old.md"]);
    assert_eq!(
        source_ids.into_iter().collect::<Vec<_>>(),
        vec!["docs/new.md"]
    );
}

/// [ORB-11695] The content-hash skip path is only worth having if it is also
/// cheap: an unchanged reindex must touch nothing but indexed lookups. Before
/// `corpus_fts` moved to external content over `chunks`, the per-source
/// stored-field probe scanned the whole corpus, so this second pass grew
/// quadratically — at this corpus size those probes alone measured ~48s.
///
/// The budget below is deliberately loose. The pass measures in the hundreds
/// of milliseconds on an unloaded debug build, but it is wall-clock on a
/// shared CI runner, so the assertion is set to catch a return to scanning
/// (two orders of magnitude) rather than to police ordinary variance.
#[test]
fn unchanged_reindex_of_a_large_corpus_stays_index_bound() {
    const DOCS: usize = 3_000;
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    let corpus = (0..DOCS)
        .map(|index| {
            doc(
                &format!("docs/note-{index:04}.md"),
                &format!("Note {index}"),
                &format!("synthetic body for note {index} about neutrino decay"),
            )
        })
        .collect::<Vec<_>>();

    let first = store.reindex_docs(&corpus, &embedder, false).unwrap();
    assert!(first.upsert.embedded_chunks >= DOCS);
    assert_eq!(first.upsert.skipped_fields, 0);

    let started = std::time::Instant::now();
    let second = store.reindex_docs(&corpus, &embedder, false).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(second.upsert.embedded_chunks, 0);
    // path, title, tags, body — every field of every doc short-circuits.
    assert_eq!(second.upsert.skipped_fields, DOCS * 4);
    assert!(
        elapsed < BUDGET,
        "unchanged reindex of {DOCS} docs took {elapsed:?}"
    );
}
