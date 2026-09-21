//! SQLite FTS5 task search and synchronous chunk indexing.
mod bm25;
mod chunker;
mod index;
mod migration;
mod store;
mod task_fields;
#[cfg(test)]
mod tests;
pub use bm25::{Bm25Hit, bm25_top_k};
pub use index::LexicalIndex;
pub use store::{LexicalStore, SearchIndexStats};
pub const SOURCE_KIND_TASK: &str = "task";
pub(crate) struct SearchField {
    pub field: String,
    pub text: String,
}
impl SearchField {
    pub fn new(field: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            text: text.into(),
        }
    }
}
