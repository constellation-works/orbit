mod candidates;
mod convert;
mod federated;
mod filters;
mod friction;
mod merge;
mod path_match;
mod runtime;
mod types;

pub use path_match::task_selectors_contain_path;
pub(crate) use types::whitespace_query_note;
pub use types::{
    GlobalSearchHit, GlobalSearchKind, GlobalSearchMode, GlobalSearchParams, GlobalSearchResponse,
};

pub(super) use merge::merge_round_robin;

const DEFAULT_LIMIT: usize = 10;
/// Upper bound on results per query. The limit comes straight from CLI and
/// MCP callers and sizes result buffers, so an unbounded value could request
/// an allocation large enough to abort the process.
const MAX_LIMIT: usize = 1_000;
