//! The config app: `orbit config` CLI commands and the dashboard's Config tab
//! reads and writes.

mod dashboard;

mod path;

mod provenance;

mod purpose;

mod read;

mod row;

mod view;

mod write;

use std::path::PathBuf;

use orbit_common::OrbitError;
use serde_json::Value;

use super::exec::{self, App, DashboardRow, Operation};
use crate::Core;
pub use dashboard::{DashboardConfigWrite, DashboardConfigWriteKind, DashboardConfigWriteResult};
pub use orbit_config::ConfigScope;
pub use orbit_runtime::refuse_indirect_uninitialized_caller_orbit;
pub use read::{
    ConfigGetResult, ConfigReadScope, ConfigShowData, ConfigShowRoots, ConfigShowWorkspaceBinding,
};
pub use view::parse_config_scope;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use view::{effective_view, file_view, key_catalog};
pub use write::{ConfigUnsetRefusal, ConfigWriteInit, ConfigWriteOutcome, GuardedConfigUnset};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use write::{
    delete_crew, set_crew, set_key, set_key_literal, unset_key, unset_key_preserving_effective,
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
    DashboardWritten(DashboardConfigWriteResult),
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
            Command::DashboardWrite(write) => return write.operation(),
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
                let outcome = write::set_key_literal(runtime, &key, &value, scope, init)?;
                Outcome::SetLiteral {
                    outcome,
                    global_root: runtime.global_root(),
                }
            }
            Command::Keys => Outcome::Keys(view::key_catalog()),
            Command::Path { global } => Outcome::Path(read::config_path(core, global)?),
            Command::EffectiveView => Outcome::View(view::effective_view(runtime)?),
            Command::FileView { scope } => Outcome::View(view::file_view(runtime, scope)?),
            Command::DashboardWrite(write) => {
                Outcome::DashboardWritten(dashboard::write(core, write)?)
            }
        })
    }

    fn dashboard_row(
        result: &Result<Outcome, crate::Error>,
        _target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        Some(dashboard::row(result, arguments))
    }
}

/// Authorize, audit and run one configuration command.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<ConfigApp>(core, command)
}
