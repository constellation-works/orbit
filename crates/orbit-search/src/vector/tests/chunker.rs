//! Unit tests for `chunker` — sibling layout under vector/tests/.

use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_common::OrbitError;

use super::super::chunker::chunk_text;
use crate::Embedder;
use crate::NoopEmbedder;

#[derive(Default)]
struct CountingEmbedder {
    calls: AtomicUsize,
}

impl CountingEmbedder {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Embedder for CountingEmbedder {
    fn model_id(&self) -> &str {
        "counting"
    }

    fn dim(&self) -> usize {
        1
    }

    fn max_input_tokens(&self) -> usize {
        512
    }

    fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        Ok(Vec::new())
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(text.split_whitespace().count().max(1))
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        Ok(word_ends(text))
    }
}

struct ContextSensitiveEmbedder;

impl Embedder for ContextSensitiveEmbedder {
    fn model_id(&self) -> &str {
        "context-sensitive"
    }

    fn dim(&self) -> usize {
        1
    }

    fn max_input_tokens(&self) -> usize {
        512
    }

    fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        Ok(Vec::new())
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        // The three words together have an extra context-dependent token.
        // Their individual counts are not safely additive.
        Ok(
            if text.contains("supercalifragilisticexpialidocious") || text == "alpha beta gamma" {
                4
            } else {
                text.split_whitespace().count().max(1)
            },
        )
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        Ok(word_ends(text))
    }
}

fn word_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut in_word = false;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if in_word {
                ends.push(index);
                in_word = false;
            }
        } else {
            in_word = true;
        }
    }
    if in_word {
        ends.push(text.len());
    }
    ends
}

#[test]
fn paragraph_chunker_overlaps_at_boundaries() {
    let embedder = NoopEmbedder::new("noop", 3, 64);
    let text = "one two three\n\nfour five six\n\nseven eight nine";
    let chunks = chunk_text(text, &embedder, 5, 3).unwrap();

    assert_eq!(chunks.len(), 3);
    assert!(chunks[0].contains("one two three"));
    assert!(chunks[1].contains("one two three"));
    assert!(chunks[1].contains("four five six"));
    assert!(chunks[2].contains("four five six"));
}

/// Flushing the buffer ahead of an over-long paragraph must reset the token
/// counter with it. It used to keep the weight of an overlap tail that was
/// then discarded, so the paragraph after the long one was flushed alone as a
/// spurious chunk and embedded twice.
#[test]
fn counter_resets_after_a_long_paragraph_is_split_on_its_own() {
    let embedder = NoopEmbedder::new("noop", 3, 64);
    let long = "l1 l2 l3 l4 l5 l6 l7 l8 l9 l10 l11 l12";
    let text = format!("a b c d\n\n{long}\n\ne f\n\ng h\n\ni j");
    let chunks = chunk_text(&text, &embedder, 5, 3).unwrap();

    assert!(
        !chunks.iter().any(|chunk| chunk == "e f"),
        "the paragraph after the split one was flushed alone: {chunks:?}"
    );
    assert!(
        chunks.iter().any(|chunk| chunk == "e f\n\ng h"),
        "short paragraphs after the split one pack together again: {chunks:?}"
    );
    assert_eq!(chunks[0], "a b c d");
}

#[test]
fn long_paragraph_uses_at_most_one_hundred_token_count_calls() {
    let embedder = CountingEmbedder::default();
    let text = (0..5_000)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");

    let chunks = chunk_text(&text, &embedder, 100, 0).unwrap();

    // The old expanding-prefix loop made 5,052 token-count calls here:
    // one whole-text count, one paragraph count, and 101 per 100-word chunk.
    // Exact token boundaries reduce the measured total to 52; keep 100 as a
    // generous regression bound for this fixture.
    assert_eq!(chunks.len(), 50);
    assert!(embedder.calls() <= 100, "calls: {}", embedder.calls());
}

#[test]
fn validates_context_sensitive_chunks_against_the_model_limit() {
    let embedder = ContextSensitiveEmbedder;
    let chunks = chunk_text("alpha beta gamma delta epsilon", &embedder, 3, 0).unwrap();

    assert!(
        chunks
            .iter()
            .all(|chunk| embedder.token_count(chunk).unwrap() <= 3)
    );
    assert_eq!(
        chunks
            .iter()
            .flat_map(|chunk| chunk.split_whitespace())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "gamma", "delta", "epsilon"]
    );
}

#[test]
fn long_paragraph_handles_unicode_empty_text_and_an_oversized_word() {
    let embedder = NoopEmbedder::new("noop", 3, 64);

    let unicode = "héllo 世界 alpha beta gamma";
    let chunks = chunk_text(unicode, &embedder, 2, 1).unwrap();
    assert!(chunks.iter().any(|chunk| chunk.contains("héllo 世界")));
    assert!(chunks.iter().any(|chunk| chunk.contains("gamma")));
    assert_eq!(
        chunk_text("", &embedder, 2, 1).unwrap(),
        vec![String::new()]
    );

    let oversized = "supercalifragilisticexpialidocious tail";
    let chunks = chunk_text(oversized, &ContextSensitiveEmbedder, 0, 0).unwrap();
    assert_eq!(chunks[0], "supercalifragilisticexpialidocious");
    assert!(chunks.iter().any(|chunk| chunk.contains("tail")));
}
