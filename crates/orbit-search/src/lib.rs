#![deny(clippy::print_stderr, clippy::print_stdout)]
#![allow(missing_docs)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]
//! SQLite FTS5 BM25 search over synchronously maintained task chunks.
mod lexical;
pub use lexical::{
    LexicalIndex, SOURCE_KIND_TASK, SearchIndexStats, TaskSidecarSearchText, bm25_or_page,
    bm25_page, bm25_top_k,
};
