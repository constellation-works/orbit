//! The consumer-facing resolved view of `config.toml`.
//!
//! [`ResolvedConfig`] is what every runtime consumer reads: admitted settings,
//! execution policies, crew registry, persistence paths, and config-owned PR
//! settings. Building one from a document also runs the migration guards for
//! retired keys, so a stale config fails (or warns) at load rather than at the
//! point of use.
//!
//! Merging the two layers into that single document is [`crate::layering`]'s
//! job; this module only ever sees one already-merged document.
//!
//! `config` assembles the admitted view; `crew` owns crew admission and lane
//! diagnostics; `compatibility` handles retired keys; `execution_env` projects
//! execution settings into subprocess policies.

mod compatibility;
mod config;
mod crew;
mod execution_env;

pub use config::ResolvedConfig;
pub use crew::disabled_crew_message;
pub use execution_env::{CodexExecutionPolicy, ExecutionEnvPolicy};

pub(crate) use compatibility::warn_compatibility_keys;
pub(crate) use crew::default_crews;

#[cfg(test)]
mod tests;
