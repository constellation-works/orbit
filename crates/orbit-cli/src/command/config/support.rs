//! Shared CLI flag type for the `orbit config` subcommand family.

use clap::ValueEnum;
use orbit_core::app::config::ConfigReadScope;

/// `--scope` value shared by `orbit config show` and `orbit config get`.
///
/// `Effective` asks callers to use the layered runtime view (see the
/// `orbit_config` module documentation). `Global`/`Workspace` always
/// read one specific file directly, without applying the other layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ConfigScopeArg {
    #[default]
    Effective,
    Global,
    Workspace,
}

impl From<ConfigScopeArg> for ConfigReadScope {
    fn from(scope: ConfigScopeArg) -> Self {
        match scope {
            ConfigScopeArg::Effective => Self::Effective,
            ConfigScopeArg::Global => Self::Global,
            ConfigScopeArg::Workspace => Self::Workspace,
        }
    }
}
