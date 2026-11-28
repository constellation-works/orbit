//! The plugin lifecycle, inspection and tool dispatch entry: install,
//! enable, disable, remove, inspect, validate, migrate and sync (design
//! `docs/design/plugins/2_design.md` §3).
//!
//! Install is global only: a plugin lives once per host under
//! `~/.orbit/plugins/<ns>/<version>/` and every workspace on that host shares
//! it. A checkout keeps only its `.orbit/plugins.yaml` pins, which git ignores
//! with the rest of `.orbit/`.
//!
//! Beyond tools, a plugin contributes definitions, seeded schedules
//! (`seed`), skills (`skill`), a `[plugins.<ns>]` config section,
//! dashboard panels (`panel`), conformance goldens (`conformance`) and
//! the names of the secrets an operator sets for it (`secret`).
//! Everything a plugin contributes is refused as a unit: a manifest whose
//! definitions break the §4.5 rules registers no tools either, because half a
//! plugin is not a state an operator can reason about.

mod build;

mod conformance;

mod dashboard;

mod inspect;

mod install;

mod lifecycle;

mod mcp_surface;

mod page;

mod panel;

mod runtime;

mod scaffold;

mod secret;

mod seed;

mod skill;

mod visibility;

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use serde_json::Value;

use super::exec::{self, App, DashboardRow, Operation};
use crate::Core;
use crate::app::exec::Caller;
use crate::application::contracts::ToolInvocationPort;
use crate::runtime::tool_exec::CapabilityEnforcement;

pub(crate) use crate::runtime::plugin::definitions::{
    load_plugin_definitions, read_definition_provenance, shipped_job_names,
};
pub(crate) use inspect::{
    PluginDoctorResult, PluginValidationReport, validate_plugin_dir,
    validate_plugin_dir_for_workspace,
};
pub(crate) use install::install_plugin_reporting_enable;
pub(crate) use lifecycle::{
    PluginEnableResult, PluginSyncOutcome, enable_plugin_from_dashboard, migrate_plugin_sidecars,
};
pub(crate) use mcp_surface::{
    FileIdentity, FileStamp, global_only_config, global_only_config_with_env,
};
pub(crate) use panel::{plugin_panel_refresh_ms, read_plugin_panel};
pub(crate) use runtime::{
    ensure_plugin_state_visible, plugin_installed_changed, stale_plugin_callback_session_count,
};
pub(crate) use seed::plugin_seeded_definitions;
pub(crate) use visibility::{InactivePlugin, PluginActivity, inactive_plugin, is_listed};

// The operations integration fixtures drive directly, public only under the
// dev-only `test-support` feature.
#[cfg(not(feature = "test-support"))]
pub(crate) use conformance::test_plugin_dir;
#[cfg(feature = "test-support")]
pub use conformance::test_plugin_dir;
#[cfg(not(feature = "test-support"))]
pub(crate) use inspect::{list_plugins, plugin_build_doctor, plugin_doctor, show_plugin};
#[cfg(feature = "test-support")]
pub use inspect::{list_plugins, plugin_build_doctor, plugin_doctor, show_plugin};
#[cfg(not(feature = "test-support"))]
pub(crate) use install::upgrade_plugin;
#[cfg(feature = "test-support")]
pub use install::{install_plugin, upgrade_plugin};
#[cfg(not(feature = "test-support"))]
pub(crate) use lifecycle::{
    disable_plugin, disable_plugin_in_workspace, enable_plugin, enable_plugin_in_workspace,
    remove_plugin, sync_plugins,
};
#[cfg(feature = "test-support")]
pub use lifecycle::{
    disable_plugin, disable_plugin_in_workspace, enable_plugin, enable_plugin_in_workspace,
    remove_plugin, sync_plugins,
};
#[cfg(not(feature = "test-support"))]
pub(crate) use runtime::add_plugin;
#[cfg(feature = "test-support")]
pub use runtime::add_plugin;
#[cfg(not(feature = "test-support"))]
pub(crate) use secret::{list_plugin_secrets, remove_plugin_secret, set_plugin_secret};
#[cfg(feature = "test-support")]
pub use secret::{list_plugin_secrets, remove_plugin_secret, set_plugin_secret};

pub use dashboard::{DashboardScope, DashboardToggle, PanelRead};
pub use page::{PageCall, PageSource};

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::adapter::command::{
    MAX_PLUGIN_SECRET_BYTES, PluginCliGroup, PluginCliVerb, PluginSecretValue,
    execute_global_plugin_tool, host_plugin_cli_groups, host_plugin_rows,
    refuse_plugin_child_cli_command, refuse_plugin_child_remote_callback,
};
pub use conformance::{PluginTestOptions, PluginTestReport, PluginTestValidationGate};
pub use inspect::{PluginSummary, PluginSummaryJsonProfile, plugin_summary_json};
pub use install::{
    PluginAddOptions, PluginPermissionChange, PluginUpgradeOptions, PluginUpgradeResult,
};
pub use lifecycle::{
    PluginEnableOptions, PluginMigrateRequest, PluginRemoveOptions, load_sidecar_manifest,
};
pub use mcp_surface::{HostPluginMcpCache, HostPluginMcpSurface};
pub use secret::PluginSecretStatus;
pub use seed::PluginSeedOutcome;
pub use skill::PluginSkillLink;

/// What `orbit plugin add --enable` produced, kept alongside the install
/// summary rather than collapsed into it, so the CLI can render seeded
/// schedules, linked skills and warnings the same way `orbit plugin enable`
/// does. `seeded`, `skills` and `warnings` are empty when `--enable` was not
/// set.
#[derive(Debug, Clone)]
pub struct PluginAddResult {
    pub summary: PluginSummary,
    pub seeded: Vec<PluginSeedOutcome>,
    pub skills: Vec<PluginSkillLink>,
    pub warnings: Vec<String>,
}

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
    /// An active page's files, for the dashboard to write into one document.
    ReadPage {
        namespace: String,
        page: String,
    },
    /// Check one call a page makes; the dashboard runs the checked call.
    CallPage {
        namespace: String,
        page: String,
        tool: String,
        input: Value,
    },
    /// A manifest-defined tool reached through the existing tool entry.
    InvokeTool(Box<ToolCall<'a>>),
    /// Check whether a plugin namespace is enabled in the bound workspace.
    WorkspaceEnabled(String),
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
    /// The toggled plugin, or `None` when no plugin has that name.
    DashboardToggled(Option<PluginSummary>),
    Panel(Box<PanelRead>),
    Page(Box<PageSource>),
    PageCall(Box<PageCall>),
    ToolOutput(Value),
    WorkspaceEnabled(bool),
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
            Command::ReadPage { .. } => {
                return Operation::dashboard_read("plugin", "page");
            }
            Command::CallPage { namespace, .. } => ("page-call", Some(namespace.clone())),
            Command::InvokeTool(call) => ("tool", Some(call.name.to_string())),
            Command::WorkspaceEnabled(_) => {
                return Operation::dashboard_read("plugin", "workspace-enabled");
            }
            Command::DashboardToggle(toggle) => return toggle.operation(),
        };
        Operation::command("plugin", verb)
            .target(Some("plugin"), target)
            .audit_role("admin")
    }

    fn run(core: &Core, command: Self::Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        let outcome = match command {
            Command::Add { source, options } => {
                Outcome::Added(add_plugin(runtime, &source, &options)?)
            }
            Command::Upgrade {
                name,
                source,
                options,
            } => Outcome::Upgraded(upgrade_plugin(runtime, &name, source.as_deref(), &options)?),
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
                enable_plugin_in_workspace(runtime, &name, options.force)?
            } else {
                enable_plugin(runtime, &name, &options)?
            }),
            Command::Disable { name, workspace } => Outcome::Disabled(if workspace {
                disable_plugin_in_workspace(runtime, &name)?
            } else {
                disable_plugin(runtime, &name)?
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
                remove_plugin(runtime, &name, &options)?;
                Outcome::Removed(state_dir)
            }
            Command::List => Outcome::Listed(list_plugins(runtime)?),
            Command::Show(name) => Outcome::Shown {
                plugin: show_plugin(runtime, &name)?,
                state_visible: ensure_plugin_state_visible(runtime).is_ok(),
            },
            Command::Doctor => {
                let results = plugin_doctor(runtime)?;
                let stale_callbacks = stale_plugin_callback_session_count(runtime)?;
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
                ensure_plugin_state_visible(runtime)?;
                Outcome::SecretSet(set_plugin_secret(runtime, &plugin, &name, &read()?)?)
            }
            Command::ListSecrets(plugin) => {
                Outcome::SecretsListed(list_plugin_secrets(runtime, &plugin)?)
            }
            Command::RemoveSecret { plugin, name } => {
                Outcome::SecretRemoved(remove_plugin_secret(runtime, &plugin, &name)?)
            }
            Command::Validate {
                dir,
                first_party,
                render,
            } => Outcome::Validated(if render {
                validate_plugin_dir_for_workspace(
                    runtime,
                    &dir,
                    first_party,
                    Some(&runtime.paths().repo_root),
                )
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
                validate_plugin_dir(runtime, &dir, first_party)?
            }),
            Command::Test { dir, options } => {
                Outcome::Tested(test_plugin_dir(runtime, &dir, &options)?)
            }
            Command::Scaffold {
                namespace,
                dir,
                force,
            } => Outcome::Scaffolded(scaffold::scaffold(&namespace, dir, force)?),
            Command::Sync { dry_run, grants } => {
                Outcome::Synced(sync_plugins(runtime, dry_run, &grants)?)
            }
            Command::Migrate(request) => {
                let (yaml, path) = migrate_plugin_sidecars(&request)?;
                Outcome::Migrated(yaml, path)
            }
            Command::ReadPanel { namespace, panel } => {
                Outcome::Panel(Box::new(PanelRead::new(core, namespace, panel)?))
            }
            Command::ReadPage { namespace, page } => {
                Outcome::Page(Box::new(page::resolve_page(core, &namespace, &page)?))
            }
            Command::CallPage {
                namespace,
                page,
                tool,
                input,
            } => Outcome::PageCall(Box::new(page::check_page_call(
                core, &namespace, &page, &tool, input,
            )?)),
            Command::InvokeTool(call) => {
                if core.caller() != Caller::Tool {
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
            Command::WorkspaceEnabled(namespace) => Outcome::WorkspaceEnabled(
                runtime
                    .ensure_plugin_enabled_in_workspace(&namespace)
                    .is_ok(),
            ),
            Command::DashboardToggle(toggle) => {
                Outcome::DashboardToggled(dashboard::toggle(core, toggle)?)
            }
        };
        Ok(outcome)
    }

    fn dashboard_row(
        result: &Result<Outcome, crate::Error>,
        target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        Some(dashboard::row(result, target, arguments))
    }

    fn exit_code(outcome: &Outcome) -> i32 {
        match outcome {
            Outcome::Tested(report) => report.exit_code(),
            Outcome::Diagnosed { issues, .. } => i32::from(*issues > 0),
            _ => 0,
        }
    }
}

/// Authorize, audit and run one plugin command.
pub fn exec(core: &Core, command: Command<'_>) -> Result<Outcome, crate::Error> {
    exec::run::<PluginApp<'_>>(core, command)
}

#[cfg(test)]
mod tests;
