//! Surgical, comment-preserving `config.toml` reads/writes for `orbit config`.
//!
//! [`ConfigStore`] wraps a single `config.toml` file as a `toml_edit::DocumentMut`
//! so `orbit config set` can edit one key without disturbing any other part of
//! the file's formatting or hand-written comments. Validation uses the same
//! layer-aware admission pipeline as runtime loads, so workspace writes see
//! global definitions and still refuse a malformed effective config.
//!
//! A store holds the bytes it was opened from. [`ConfigStore::save`] rereads
//! the file under the file's write lock and refuses to replace content that
//! changed after the store was opened, so a stale edit cannot erase another
//! writer's successful one. Writers that open, edit, validate and save run
//! that whole sequence inside [`ConfigStore::with_write_lock`], so concurrent
//! edits of one file serialize instead of refusing each other.

mod document;
mod persistence;
mod validation;

pub use persistence::{ConfigScope, ConfigStore, WorkspaceInitMode};
