#![deny(clippy::print_stderr, clippy::print_stdout)]
#![allow(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! SQLite FTS5 BM25 search over synchronously maintained task chunks.
mod lexical;
pub use lexical::{
    Bm25Hit, LexicalIndex, LexicalStore, SOURCE_KIND_TASK, SearchIndexStats, bm25_top_k,
};
