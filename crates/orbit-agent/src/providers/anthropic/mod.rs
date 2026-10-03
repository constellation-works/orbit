//! Anthropic Messages API HTTP transport.
//!
//! Implements the [`LoopTransport`](crate::loop_engine::LoopTransport) trait
//! against `https://api.anthropic.com/v1/messages`. Sibling to — not a
//! replacement for — the CLI-based `claude_runtime` in this crate.

mod messages_transport;
mod wire;

pub use messages_transport::AnthropicMessagesTransport;
