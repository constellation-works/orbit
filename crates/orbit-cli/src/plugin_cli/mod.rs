//! The `orbit <ns> <verb>` command groups an installed plugin contributes
//! (design `docs/design/plugins/1_scope.md` §4.6).
//!
//! These commands are not in the `Commands` enum: their names and flags come
//! from the manifests this host has enabled, so the clap tree is finished at
//! startup rather than at compile time. What they *do* is not new — every
//! group reduces to the `orbit tool run <ns>.<verb>` the operator could have
//! typed instead ([`command`]), and the mapping from a tool's JSON Schema to
//! its flags lives in [`schema`].

mod command;
mod schema;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use command::PLUGIN_HELP_HEADING;
pub(crate) use command::{PluginGroupInvocation, augment, help_section, invocation_from_matches};
