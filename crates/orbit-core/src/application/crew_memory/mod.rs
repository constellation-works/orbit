//! Crew memory (experimental, off by default): per-model memory spaces and
//! the global memory, held as append-only revisions in the owner store.
//!
//! Slice M1 holds the store, the write rules and the operator surface. Every
//! writer goes through one write path, which refuses while
//! `crew_memory.enabled` is false and runs [`validate_memory_write`], the one
//! place the five write rules live. Nothing here delivers memory to a
//! prompt on its own: slice M2's `memory` experiment overlay renders a space
//! with [`OrbitRuntime::render_crew_memory_delivery`](crate::OrbitRuntime)
//! for a memory arm's dispatches.
//! Design: `docs/design/crew-memory/2_design.md`.

mod delivery;
mod key;
mod operations;
mod validate;

use serde::{Deserialize, Serialize};

pub use key::{GLOBAL_MEMORY_KEY, MemoryKey, memory_key, parse_memory_key};
pub use validate::{
    GLOBAL_MAX_BYTES, GLOBAL_MAX_FILES, MODEL_SPACE_MAX_BYTES, MODEL_SPACE_MAX_FILES,
    MemoryLintVocabulary, MemoryRule, MemoryWriteContext, MemoryWriteRefusal,
    validate_memory_write,
};

/// One Markdown file of a memory space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryFile {
    /// The file name, `[a-z0-9-]{1,40}.md`.
    pub path: String,
    pub text: String,
}

/// A write's whole new content and the evidence it cites. A write replaces
/// every file of the space; there is no append.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryWrite {
    pub files: Vec<MemoryFile>,
    /// Evidence task ids the notes rest on.
    pub evidence: Vec<String>,
}

/// Who writes a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryWriter {
    /// An operator through the CLI. May cite no evidence.
    Operator { actor: String },
    /// A managed run (a dreaming run, from slice M3): it must cite evidence,
    /// and it writes only the space of its own recorded `crew_model`.
    Run {
        run_id: String,
        crew: String,
        actor: String,
    },
}

impl MemoryWriter {
    /// `operator` or `run`, as stored.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Operator { .. } => "operator",
            Self::Run { .. } => "run",
        }
    }

    /// The label stored as the revision's and event's actor.
    pub fn actor(&self) -> &str {
        match self {
            Self::Operator { actor } | Self::Run { actor, .. } => actor,
        }
    }
}
