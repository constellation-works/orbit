#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy semantic-indexing surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Slim search client surface for Orbit semantic indexing.
//!
//! This crate intentionally contains no inference backend. The main `orbit`
//! binary links this crate, locates the separately installed companion binary,
//! and speaks a small JSON-Lines RPC protocol over stdio.
//!
//! Module layout — start with the entry point that matches your need:
//!
//! - [`embedder`] — the [`Embedder`] trait + [`ModelSpec`] catalog. Read first
//!   if you're integrating a new caller; everything else is downstream of this.
//! - [`rpc`] — the JSON-Lines protocol shared with `orbit-search-companion`.
//! - [`companion`] — discovery of the installed companion binary.
//! - [`noop`] — a deterministic test fake that needs no companion subprocess.
//! - [`subprocess`] — the production [`Embedder`] impl that talks to the
//!   companion over stdio.
//! - [`vector`] — workspace-local SQLite storage for embeddings + FTS5 rows.
//! - [`commands`] — install / uninstall / reindex / stats command surface.

mod commands;
mod companion;
mod embedder;
pub mod lexical;
mod noop;
mod rpc;
mod subprocess;
mod vector;

#[cfg(test)]
mod tests;

pub use commands::{
    CompanionStatus, DocIndexParams, DocIndexResult, DocSemanticHit, DocSemanticSearchParams,
    DocSemanticSearchResult, IndexKind, ScoreBreakdown, SemanticHit, SemanticIndexParams,
    SemanticIndexResult, SemanticInstallParams, SemanticInstallResult, SemanticReindexParams,
    SemanticReindexResult, SemanticRelatedParams, SemanticRelatedResult, SemanticSearchParams,
    SemanticSearchResult, SemanticStatsResult, SemanticUninstallParams, SemanticUninstallResult,
    TaskIndexResult, doc_index, doc_semantic_search, query_model_id, semantic_index,
    semantic_install, semantic_reindex, semantic_related, semantic_search, semantic_stats,
    semantic_uninstall,
};
pub use companion::{
    CompanionPaths, INSTALL_REMEDIATION, locate_companion, platform_companion_filename, platform_id,
};
pub use embedder::{DEFAULT_MODEL, Embedder, ModelSpec, default_model, supported_models};
pub use lexical::docs::{
    DocSearchResult, DocSearchSource, SearchResult, score_doc_record, sort_search_results,
};
pub use noop::NoopEmbedder;
pub use rpc::{
    RpcError, RpcRequest, RpcResponse, RpcResult, UNCORRELATED_REQUEST_ID, rpc_error_to_orbit,
    unparsed_request_id,
};
pub use subprocess::SubprocessEmbedder;
pub use vector::{
    DocEmbeddingSource, EmbedWorker, SOURCE_KIND_DOC, SOURCE_KIND_TASK, SemanticStats,
    UpsertReport, VectorStore,
};
