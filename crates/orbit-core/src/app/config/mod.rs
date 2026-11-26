//! The config app: `orbit config` CLI commands and the dashboard's Config tab
//! reads and writes.

mod dashboard;
mod read;

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::AuthorizationDenial;
use serde_json::Value;

use super::exec::{self, App, Operation};
use crate::Core;
pub use crate::application::config::{
    ConfigScope, ConfigUnsetRefusal, ConfigWriteInit, ConfigWriteOutcome, GuardedConfigUnset,
    parse_config_scope,
};
pub use crate::runtime::refuse_indirect_uninitialized_caller_orbit;
pub use dashboard::{DashboardConfigWrite, DashboardConfigWriteKind, DashboardConfigWriteResult};
pub use read::{
    ConfigGetResult, ConfigReadScope, ConfigShowData, ConfigShowRoots, ConfigShowWorkspaceBinding,
};

/// One configuration command, shared by the CLI and dashboard surfaces.
pub enum Command {
    /// Read the layered effective configuration or one file in isolation for
    /// `orbit config show`.
    Show {
        /// Which layer view to load.
        scope: ConfigReadScope,
    },
    /// Read one configuration key for `orbit config get`.
    Get {
        /// Dotted config key.
        key: String,
        /// Which layer view to read.
        scope: ConfigReadScope,
    },
    /// Set one configuration key from a CLI TOML literal.
    SetLiteral {
        /// Dotted config key.
        key: String,
        /// Raw literal or plain string value.
        value: String,
        /// Target file scope.
        scope: ConfigScope,
        /// First-write initialization policy for workspace scope.
        init: ConfigWriteInit,
    },
    /// Return the static configuration key catalog.
    Keys,
    /// Resolve the active or global `config.toml` path.
    Path {
        /// Whether to return the global `config.toml` path instead of the
        /// effective one.
        global: bool,
    },
    /// Project the layered effective configuration view for the dashboard.
    EffectiveView,
    /// Project one physical configuration file view for the dashboard.
    FileView {
        /// Target file scope.
        scope: ConfigScope,
    },
    /// Apply one authorized, audited dashboard configuration write.
    DashboardWrite(DashboardConfigWrite),
}

/// What a configuration command produced, before the caller renders it.
pub enum Outcome {
    /// Data for `orbit config show`.
    Shown(Box<ConfigShowData>),
    /// Data for `orbit config get`.
    Got(ConfigGetResult),
    /// Applied CLI `orbit config set` write plus the host global root for
    /// service-environment warnings.
    SetLiteral {
        /// Applied write outcome.
        outcome: ConfigWriteOutcome,
        /// Host global root for post-write service-environment checks.
        global_root: PathBuf,
    },
    /// Static configuration key catalog.
    Keys(&'static Value),
    /// Resolved `config.toml` path.
    Path(PathBuf),
    /// Projected dashboard configuration view.
    View(Value),
    /// Applied or refused dashboard configuration write.
    DashboardWritten(Result<DashboardConfigWriteResult, AuthorizationDenial>),
}

/// Why a configuration command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The flow failed.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct ConfigApp;

impl App for ConfigApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        let subcommand = match command {
            Command::Show { .. } => "show",
            Command::Get { .. } => "get",
            Command::SetLiteral { .. } => "set",
            Command::Keys => "keys",
            Command::Path { .. } => "path",
            Command::EffectiveView => "effective-view",
            Command::FileView { .. } => "file-view",
            Command::DashboardWrite(_) => {
                unreachable!("dashboard config writes use dashboard admission")
            }
        };
        Operation::command("config", subcommand).target(Some("config"), None)
    }

    fn run(core: &Core, command: Self::Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::Show { scope } => Outcome::Shown(Box::new(read::show(core, scope)?)),
            Command::Get { key, scope } => Outcome::Got(read::get(core, &key, scope)?),
            Command::SetLiteral {
                key,
                value,
                scope,
                init,
            } => {
                let outcome = crate::application::config::set_key_literal(
                    runtime, &key, &value, scope, init,
                )?;
                Outcome::SetLiteral {
                    outcome,
                    global_root: runtime.global_root(),
                }
            }
            Command::Keys => Outcome::Keys(crate::application::config::key_catalog()),
            Command::Path { global } => Outcome::Path(read::config_path(core, global)?),
            Command::EffectiveView => {
                Outcome::View(crate::application::config::effective_view(runtime)?)
            }
            Command::FileView { scope } => {
                Outcome::View(crate::application::config::file_view(runtime, scope)?)
            }
            Command::DashboardWrite(_) => {
                unreachable!("dashboard config writes use dashboard admission")
            }
        })
    }
}

/// Authorize, audit and run one configuration command.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    match command {
        Command::DashboardWrite(write) => dashboard::exec(core, write),
        command => exec::run::<ConfigApp>(core, command),
    }
}
