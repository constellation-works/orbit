//! The resident mandate CLI, derived from the resident operation registry.
//!
//! Every verb calls its `orbit.resident.*` tool, so `--json` is the tool's
//! payload; what is left here is the text rendering.

mod checkpoint;
mod command;
mod control;
mod digest;
mod grant;
mod list;
mod metrics;
mod relay;
mod show;
mod support;

pub use command::ResidentCommand;
