use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_common::OrbitError;

use crate::{Embedder, NoopEmbedder, SharedQueryEmbedder};

/// A `NoopEmbedder` that counts how often the real embed call was reached.
#[derive(Debug, Default)]
struct CountingEmbedder {
    inner: NoopEmbedder,
    calls: Arc<AtomicUsize>,
}

impl CountingEmbedder {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inner: NoopEmbedder::small(),
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

impl Embedder for CountingEmbedder {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.embed(texts)
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        self.inner.token_count(text)
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        self.inner.token_boundaries(text)
    }
}

#[test]
fn a_repeated_query_is_embedded_once_and_answered_identically() {
    let (counting, calls) = CountingEmbedder::new();
    let shared = SharedQueryEmbedder::new(Arc::new(counting));

    let first = shared.embed(&["federated query"]).expect("first embed");
    let second = shared.embed(&["federated query"]).expect("memoized embed");

    assert_eq!(first, second, "the memo must not change the answer");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the second reader must not re-embed the same query"
    );
}

#[test]
fn a_different_batch_is_embedded_again() {
    let (counting, calls) = CountingEmbedder::new();
    let shared = SharedQueryEmbedder::new(Arc::new(counting));

    shared.embed(&["first"]).expect("first embed");
    shared.embed(&["second"]).expect("second embed");
    // Same texts, different batch shape: the memo must not answer this one.
    shared.embed(&["second", "second"]).expect("third embed");

    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn concurrent_readers_of_one_query_share_a_single_embedding() {
    let (counting, calls) = CountingEmbedder::new();
    let shared = SharedQueryEmbedder::new(Arc::new(counting));

    let vectors = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| scope.spawn(|| shared.embed(&["fan out"]).expect("embed")))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .collect::<Vec<_>>()
    });

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(vectors.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn the_wrapper_reports_the_inner_model() {
    let shared = SharedQueryEmbedder::new(Arc::new(NoopEmbedder::new("minilm-l6", 4, 128)));

    assert_eq!(shared.model_id(), "minilm-l6");
    assert_eq!(shared.dim(), 4);
    assert_eq!(shared.max_input_tokens(), 128);
    assert_eq!(shared.token_count("two words").expect("tokens"), 2);
}
