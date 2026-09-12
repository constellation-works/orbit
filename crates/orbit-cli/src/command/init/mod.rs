//! `orbit init` and the host-facing adapter that feeds it.
//!
//! Configuration itself is owned by `orbit-config`, which is deliberately
//! host-blind: it neither probes `PATH` nor touches a terminal. The two pieces
//! that do live here, next to the command that needs them —
//! [`agent_detect`] discovers which provider CLIs are installed, and
//! [`agent_prompt`] collects crew choices from stdin — and their results are
//! handed down as an explicit `orbit_config::ConfigSeed`.

pub mod agent_detect;
pub mod agent_prompt;
mod command;
mod prompt_stdin;
mod seed;

pub use command::InitCommand;
pub(crate) use seed::{collect_config_seed_for_init, config_seed_from_detection};

#[cfg(test)]
mod tests;
