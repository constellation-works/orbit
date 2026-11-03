//! The read-only attention operation, shared by CLI and MCP, and the
//! semantics every responder applies: filters, the delta cursor and the
//! public page.
pub mod cursor;
pub mod filter;
pub mod page;

mod refusals;
mod registry;

pub use refusals::{attention_candidate_read_refusal, attention_scope_refusal};
pub use registry::{ATTENTION, ATTENTION_AGGREGATOR_CLIENT, ATTENTION_OPERATIONS, AttentionVerb};
