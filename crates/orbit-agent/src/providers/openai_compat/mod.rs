//! OpenAI-compatible chat-completions HTTP transport.
//!
//! Implements the [`LoopTransport`](crate::loop_engine::LoopTransport) trait
//! against OpenAI-compatible `POST /v1/chat/completions` endpoints, including
//! hosted OpenAI and local servers that emulate the same schema.

mod chat_completions_transport;
mod wire;

pub use chat_completions_transport::OpenAiCompatTransport;
