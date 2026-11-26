//! The plugin lifecycle, inspection and tool dispatch entry.

mod dashboard;
mod scaffold;

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::AuthorizationDenial;
use orbit_tools::ToolContext;
use serde_json::Value;

use super::exec::{self, App, Operation};
use crate::Core;
use crate::application::contracts::ToolInvocationPort;
use crate::application::plugin::{
    PluginAddOptions, PluginAddResult, PluginDoctorResult, PluginEnableOptions, PluginEnableResult,
    PluginMigrateRequest, PluginRemoveOptions, PluginSecretStatus, PluginSummary,
    PluginSyncOutcome, PluginTestOptions, PluginTestReport, PluginUpgradeOptions,
    PluginUpgradeResult, PluginValidationReport, migrate_plugin_sidecars,
};
use crate::composition::Entry;
use crate::runtime::plugin::secrets::PluginSecretValue;
use crate::runtime::tool_exec::CapabilityEnforcement;

pub use dashboard::{DashboardScope, DashboardToggle, PanelRead};

/// One plugin command, shared by the surfaces that invoke it.
pub enum Command<'a> {
    Add {
        source: String,
        options: PluginAddOptions,
    },
    Upgrade {
        name: String,
        source: Option<String>,
        options: PluginUpgradeOptions,
    },
    Enable {
        name: String,
        options: PluginEnableOptions,
        workspace: bool,
    },
    Disable {
        name: String,
        workspace: bool,
    },
    Remove {
        name: String,
        options: PluginRemoveOptions,
        confirmed: bool,
    },
    List,
    Show(String),
    Doctor,
    SetSecret {
        plugin: String,
        name: String,
        /// Reads the value after the app has checked state visibility.
        read: &'a dyn Fn() -> Result<PluginSecretValue, OrbitError>,
        trailing_args: bool,
    },
    ListSecrets(String),
    RemoveSecret {
        plugin: String,
        name: String,
    },
    Validate {
        dir: PathBuf,
        first_party: bool,
        render: bool,
    },
    Test {
        dir: PathBuf,
        options: PluginTestOptions,
    },
    Scaffold {
        namespace: String,
        dir: Option<PathBuf>,
        force: bool,
    },
    Sync {
        dry_run: bool,
        grants: Vec<String>,
    },
    Migrate(PluginMigrateRequest),
    DashboardToggle(DashboardToggle),
    ReadPanel {
        namespace: String,
        panel: String,
    },
    /// A manifest-defined tool reached through the existing tool entry.
    InvokeTool(Box<ToolCall<'a>>),
}

/// Tool arguments and a context assembled by the audited tool entry.
pub struct ToolCall<'a> {
    pub(crate) name: &'a str,
    pub(crate) input: Value,
    pub(crate) context: ToolContext,
    pub(crate) enforcement: CapabilityEnforcement,
}

/// A scaffold's generated source tree.
pub struct Scaffold {
    pub namespace: String,
    pub source: String,
    pub root: String,
    pub files: Vec<String>,
}

/// What the command produced, before the caller renders it.
pub enum Outcome {
    Added(PluginAddResult),
    Upgraded(PluginUpgradeResult),
    Enabled(PluginEnableResult),
    Disabled(PluginSummary),
    Removed(PathBuf),
    Listed(Vec<PluginSummary>),
    Shown {
        plugin: PluginSummary,
        state_visible: bool,
    },
    Diagnosed {
        results: Vec<PluginDoctorResult>,
        stale_callbacks: usize,
        issues: usize,
    },
    SecretSet(PluginSecretStatus),
    SecretsListed(Vec<PluginSecretStatus>),
    SecretRemoved(bool),
    Validated(PluginValidationReport),
    Tested(PluginTestReport),
    Scaffolded(Scaffold),
    Synced(Vec<PluginSyncOutcome>),
    Migrated(String, Option<PathBuf>),
    DashboardToggled(Result<Option<PluginSummary>, AuthorizationDenial>),
    Panel(Box<PanelRead>),
    ToolOutput(Value),
}

/// Why a plugin command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
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

/// Authorize, audit and run one plugin command.
pub fn exec(core: &Core, command: Command<'_>) -> Result<Outcome, crate::Error> {
    match command {
        Command::DashboardToggle(toggle) => dashboard::exec(core, toggle),
        command => exec::run::<PluginApp<'_>>(core, command),
    }
}

struct PluginApp<'a>(std::marker::PhantomData<&'a ()>);

impl<'a> App for PluginApp<'a> {
    type Command = Command<'a>;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        let (verb, target) = match command {
            Command::Add { source, .. } => ("add", Some(source.clone())),
            Command::Upgrade { name, .. } => ("upgrade", Some(name.clone())),
            Command::Enable { name, .. } => ("enable", Some(name.clone())),
            Command::Disable { name, .. } => ("disable", Some(name.clone())),
            Command::Remove { name, .. } => ("remove", Some(name.clone())),
            Command::List => ("list", None),
            Command::Show(name) => ("show", Some(name.clone())),
            Command::Doctor => ("doctor", None),
            Command::SetSecret { plugin, .. } => ("secret-set", Some(plugin.clone())),
            Command::ListSecrets(plugin) => ("secret-list", Some(plugin.clone())),
            Command::RemoveSecret { plugin, .. } => ("secret-rm", Some(plugin.clone())),
            Command::Validate { dir, .. } => ("validate", Some(dir.to_string_lossy().into_owned())),
            Command::Test { dir, .. } => ("test", Some(dir.to_string_lossy().into_owned())),
            Command::Scaffold { namespace, .. } => ("scaffold", Some(namespace.clone())),
            Command::Sync { .. } => ("sync", None),
            Command::Migrate(request) => ("migrate", Some(request.backend_command.clone())),
            Command::ReadPanel { namespace, .. } => ("panel", Some(namespace.clone())),
            Command::InvokeTool(call) => ("tool", Some(call.name.to_string())),
            Command::DashboardToggle(_) => {
                unreachable!("dashboard toggles use dashboard admission")
            }
        };
        Operation::command("plugin", verb)
            .target(Some("plugin"), target)
            .audit_role("admin")
    }

    fn run(core: &Core, command: Self::Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        let outcome = match command {
            Command::Add { source, options } => {
                Outcome::Added(runtime.add_plugin(&source, &options)?)
            }
            Command::Upgrade {
                name,
                source,
                options,
            } => Outcome::Upgraded(runtime.upgrade_plugin(&name, source.as_deref(), &options)?),
            Command::Enable {
                name,
                options,
                workspace,
            } => Outcome::Enabled(if workspace {
                if !options.grants.is_empty() {
                    return Err(OrbitError::InvalidInput(
                            "--grant is only valid with host scope; workspace scope never records grants"
                                .into(),
                        ).into());
                }
                runtime.enable_plugin_in_workspace(&name, options.force)?
            } else {
                runtime.enable_plugin(&name, &options)?
            }),
            Command::Disable { name, workspace } => Outcome::Disabled(if workspace {
                runtime.disable_plugin_in_workspace(&name)?
            } else {
                runtime.disable_plugin(&name)?
            }),
            Command::Remove {
                name,
                options,
                confirmed,
            } => {
                if !confirmed {
                    return Err(OrbitError::InvalidInput(format!(
                        "removing plugin '{name}' is irreversible; pass --confirm to proceed"
                    ))
                    .into());
                }
                let state_dir = runtime.global_root().join("state/plugins").join(&name);
                runtime.remove_plugin(&name, &options)?;
                Outcome::Removed(state_dir)
            }
            Command::List => Outcome::Listed(runtime.list_plugins()?),
            Command::Show(name) => Outcome::Shown {
                plugin: runtime.show_plugin(&name)?,
                state_visible: runtime.ensure_plugin_state_visible().is_ok(),
            },
            Command::Doctor => {
                let results = runtime.plugin_doctor()?;
                let stale_callbacks = runtime.stale_plugin_callback_session_count()?;
                let issues = results
                    .iter()
                    .filter(|result| !result.intentional && !result.message.is_empty())
                    .count()
                    + usize::from(stale_callbacks > 0);
                Outcome::Diagnosed {
                    results,
                    stale_callbacks,
                    issues,
                }
            }
            Command::SetSecret {
                plugin,
                name,
                read,
                trailing_args,
            } => {
                if trailing_args {
                    return Err(OrbitError::InvalidInput(format!(
                        "a secret value is never taken from the command line, where other processes and \
                         shell history can read it; pipe it on stdin (`orbit plugin secret set {plugin} {name} < \
                         file`) or run the command in a terminal to be prompted"
                    )).into());
                }
                runtime.ensure_plugin_state_visible()?;
                Outcome::SecretSet(runtime.set_plugin_secret(&plugin, &name, &read()?)?)
            }
            Command::ListSecrets(plugin) => {
                Outcome::SecretsListed(runtime.list_plugin_secrets(&plugin)?)
            }
            Command::RemoveSecret { plugin, name } => {
                Outcome::SecretRemoved(runtime.remove_plugin_secret(&plugin, &name)?)
            }
            Command::Validate {
                dir,
                first_party,
                render,
            } => Outcome::Validated(if render {
                runtime.validate_plugin_dir_rendered(&dir, first_party, &runtime.paths().repo_root)
                        .map_err(|error| {
                            if runtime.shared_root() == runtime.global_root() {
                                OrbitError::InvalidInput(format!(
                                    "{error}; use --workspace <SELECTOR> if rendering needs workspace config"
                                ))
                            } else {
                                error
                            }
                        })?
            } else {
                runtime.validate_plugin_dir(&dir, first_party)?
            }),
            Command::Test { dir, options } => {
                Outcome::Tested(runtime.test_plugin_dir(&dir, &options)?)
            }
            Command::Scaffold {
                namespace,
                dir,
                force,
            } => Outcome::Scaffolded(scaffold::scaffold(&namespace, dir, force)?),
            Command::Sync { dry_run, grants } => {
                Outcome::Synced(runtime.sync_plugins(dry_run, &grants)?)
            }
            Command::Migrate(request) => {
                let (yaml, path) = migrate_plugin_sidecars(&request)?;
                Outcome::Migrated(yaml, path)
            }
            Command::ReadPanel { namespace, panel } => {
                Outcome::Panel(Box::new(PanelRead::new(core, namespace, panel)?))
            }
            Command::InvokeTool(call) => {
                if core.entry() != Entry::Tool {
                    return Err(OrbitError::PolicyDenied(
                        "plugin tools must use the tool entry".into(),
                    )
                    .into());
                }
                Outcome::ToolOutput(runtime.run_tool_with_enforcement(
                    call.name,
                    call.input,
                    call.context,
                    call.enforcement,
                )?)
            }
            Command::DashboardToggle(_) => {
                unreachable!("dashboard toggles use dashboard admission")
            }
        };
        Ok(outcome)
    }

    fn exit_code(outcome: &Outcome) -> i32 {
        match outcome {
            Outcome::Tested(report) => report.exit_code(),
            Outcome::Diagnosed { issues, .. } => i32::from(*issues > 0),
            _ => 0,
        }
    }
}
