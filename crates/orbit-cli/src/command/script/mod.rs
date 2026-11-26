//! `orbit script` — grant and inspect the workspace scripts a job step
//! may name with `target: script:<name>`.
//!
//! The consent surface: manifests under `.orbit/resources/scripts/`, an
//! operator grant bound to the manifest and entry digests, the staleness
//! reporting that shows when either file changed after granting, and the
//! history of what ran. Grant and revoke are governed operator-only
//! operations; an agent session can list, show and read history, never
//! grant the code it can edit.

mod command;
mod grant;
mod history;
mod list;
mod revoke;
mod show;
mod support;

pub use command::{ScriptCommand, ScriptSubcommand};
