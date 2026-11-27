//! The fleet notice CLI, derived from the notice operation registry.
//!
//! Every verb calls its `orbit.notice.*` tool, so `--json` is the tool's
//! payload. What stays here is turning the CLI-only flags (`--body-file`,
//! `--keep-after-done`, `--evidence TYPE=VALUE`) into tool input; rendering
//! lives in `crate::output::notice`.

mod command;
mod post;

pub use command::NoticeCommand;
pub(crate) use post::post_cli_input;
