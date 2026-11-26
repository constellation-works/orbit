//! `orbit config browser-kit` — the headless browser kit workers check UIs with.
//!
//! Each subcommand calls `orbit_cmd::browser_kit` and opens no workspace
//! runtime: the kit belongs to the host user, not to a repository.

mod command;
mod install;
mod remove;
mod status;
mod support;
mod verify;

pub use command::{BrowserKitCommand, BrowserKitSubcommand};
