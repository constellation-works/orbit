//! The fleet notice CLI, derived from the notice operation registry.
//!
//! Every verb calls its `orbit.notice.*` tool, so `--json` is the tool's
//! payload. What is left here is turning the CLI-only flags (`--body-file`,
//! `--keep-after-done`, `--evidence TYPE=VALUE`) into tool input, and the text
//! rendering: a table for `list`, the notice for every other verb.

mod close;
mod command;
mod list;
mod post;
mod show;
mod support;

pub use command::NoticeCommand;
pub(crate) use command::render_notice;
pub(crate) use post::post_cli_input;
