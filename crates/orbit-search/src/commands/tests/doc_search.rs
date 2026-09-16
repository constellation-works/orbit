//! Unit tests for `doc_search` — sibling layout under commands/tests/.

use super::super::doc_search::{DocSemanticSearchParams, doc_lexical_search, run_with_embedder};

use crate::vector::{DocEmbeddingSource, VectorStore};
use crate::{Embedder, NoopEmbedder};
use orbit_common::OrbitError;

struct KeywordEmbedder;

impl Embedder for KeywordEmbedder {
    fn model_id(&self) -> &str {
        "keyword"
    }

    fn dim(&self) -> usize {
        2
    }

    fn max_input_tokens(&self) -> usize {
        512
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        Ok(texts
            .iter()
            .map(|text| {
                if text.to_ascii_lowercase().contains("concept") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        Ok(text.split_whitespace().count().max(1))
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        Ok(text
            .match_indices(char::is_whitespace)
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .collect())
    }
}

fn doc(path: &str, body: &str) -> DocEmbeddingSource {
    DocEmbeddingSource {
        path: path.to_string(),
        title: path.to_string(),
        tags: Vec::new(),
        body: body.to_string(),
    }
}

#[test]
fn doc_semantic_search_filters_to_doc_rows() {
    let store = VectorStore::open_in_memory().unwrap();
    let doc_embedder = KeywordEmbedder;
    store
        .reindex_docs(
            &[
                doc("docs/concept.md", "concept match"),
                doc("docs/other.md", "other body"),
            ],
            &doc_embedder,
            false,
        )
        .unwrap();
    store
        .upsert_embeddings(
            "task",
            "ORB-00000",
            &[crate::vector::EmbeddingField::new("title", "concept task")],
            &NoopEmbedder::small(),
            false,
        )
        .unwrap();

    let result = run_with_embedder(
        &store,
        &doc_embedder,
        DocSemanticSearchParams {
            query: "concept".to_string(),
            limit: 1,
            model: None,
        },
    )
    .unwrap();

    assert_eq!(result.results[0].source_id, "docs/concept.md");
}

/// [DANI-10369] The lexical half of hybrid doc search reads `corpus_fts`:
/// every matching chunk rolls up to its doc, docs keep BM25 order, and the
/// snippet is the best chunk's stored text — nothing is read from disk.
#[test]
fn doc_lexical_search_rolls_chunks_up_to_docs_in_bm25_order() {
    let store = VectorStore::open_in_memory().unwrap();
    let embedder = NoopEmbedder::small();
    store
        .reindex_docs(
            &[
                doc("docs/twice.md", "neutrino here and neutrino there"),
                doc("docs/once.md", "one neutrino mention"),
                doc("docs/never.md", "nothing relevant"),
            ],
            &embedder,
            false,
        )
        .unwrap();
    store
        .upsert_embeddings(
            "task",
            "ORB-00000",
            &[crate::vector::EmbeddingField::new("title", "neutrino task")],
            &embedder,
            false,
        )
        .unwrap();

    let hits = doc_lexical_search(&store, "neutrino", 10).unwrap();

    let ids = hits
        .iter()
        .map(|hit| hit.source_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["docs/twice.md", "docs/once.md"]);
    assert_eq!(
        hits.iter().map(|hit| hit.rank).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(hits[0].best_field, "body");
    assert_eq!(hits[0].snippet, "neutrino here and neutrino there");

    assert_eq!(doc_lexical_search(&store, "neutrino", 1).unwrap().len(), 1);
    assert!(doc_lexical_search(&store, "   ", 10).unwrap().is_empty());
}
