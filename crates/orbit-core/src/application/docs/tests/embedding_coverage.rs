//! Doc embedding coverage powering the `docs-index` doctor check [ORB-12259].

use std::fs;

use orbit_search::{DocEmbeddingSource, NoopEmbedder};

use crate::OrbitRuntime;

fn write_doc(runtime: &OrbitRuntime, path: &str, summary: &str) {
    let doc_path = runtime.paths().repo_root.join(path);
    fs::create_dir_all(doc_path.parent().expect("doc parent")).expect("create doc parent");
    fs::write(
        doc_path,
        format!("---\ntype: context\nsummary: {summary}\n---\n\nbody\n"),
    )
    .expect("write doc");
}

fn embed_doc(runtime: &OrbitRuntime, path: &str, title: &str) {
    let store = runtime
        .stores()
        .semantic_index()
        .store()
        .expect("open vector store");
    store
        .index_doc(
            &DocEmbeddingSource {
                path: path.to_string(),
                title: title.to_string(),
                tags: Vec::new(),
                body: "body".to_string(),
            },
            &NoopEmbedder::small(),
            false,
        )
        .expect("index doc");
}

#[test]
fn no_docs_corpus_reports_zero_total() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let coverage = runtime.docs_embedding_coverage().expect("coverage");
    assert_eq!(coverage.total_sources, 0);
    assert_eq!(coverage.embedded_sources, 0);
}

#[test]
fn unembedded_docs_are_not_counted_as_coverage() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    write_doc(&runtime, "docs/a.md", "doc a");
    write_doc(&runtime, "docs/b.md", "doc b");

    let coverage = runtime.docs_embedding_coverage().expect("coverage");
    assert_eq!(coverage.total_sources, 2);
    assert_eq!(coverage.embedded_sources, 0);
}

#[test]
fn partially_embedded_docs_are_counted_precisely() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    write_doc(&runtime, "docs/a.md", "doc a");
    write_doc(&runtime, "docs/b.md", "doc b");
    embed_doc(&runtime, "docs/a.md", "doc a");

    let coverage = runtime.docs_embedding_coverage().expect("coverage");
    assert_eq!(coverage.total_sources, 2);
    assert_eq!(coverage.embedded_sources, 1);
}

#[test]
fn stale_embedding_for_a_removed_doc_does_not_count_as_coverage() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    write_doc(&runtime, "docs/a.md", "doc a");
    embed_doc(&runtime, "docs/a.md", "doc a");
    embed_doc(&runtime, "docs/removed.md", "removed doc");

    let coverage = runtime.docs_embedding_coverage().expect("coverage");
    assert_eq!(coverage.total_sources, 1);
    assert_eq!(coverage.embedded_sources, 1);
}
