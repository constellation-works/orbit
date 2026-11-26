//! The workspace app: the host workspace registry commands of
//! `orbit workspace`, the dashboard's fleet task reads, and the
//! workspace-claim tool verbs.
//!
//! A registry command reads or writes the host workspace registry for the
//! workspace this `Core` is bound to. Removal and teardown are run by the
//! caller's own flow, which the app runs once it has admitted the command:
//! that flow lives above this crate.

mod registry;
mod tasks;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use orbit_config::WorkspaceSeedingPolicies;
use orbit_registry::workspace_registry::{WorkspaceShipModeRebind, WorkspaceSourceRemoteRebind};
use orbit_types::task::TaskStatus;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspacePublicationBinding,
    WorkspaceRegistry,
};
use serde_json::Value;

use super::exec::{self, App, Operation};
use crate::application::seeding_policy::SeedingPolicyExclusions;
use crate::application::task::{TaskCandidateKeys, TaskListFilter, TaskRow};
use crate::application::workspace_preset::AppliedWorkspacePreset;
use crate::{Core, ShipMode};

/// The caller's own flow for a command whose body lives above this crate. It
/// is called once, with the host's global root, after the command is
/// admitted.
pub type HostFlow<'a> = &'a mut dyn FnMut(&Path) -> Result<(), OrbitError>;

/// One workspace command, shared by the surfaces that invoke it.
pub enum Command<'a> {
    /// List the registered workspaces.
    List,
    /// Show the workspace bound to the current checkout.
    Show,
    /// Show the bound workspace's registered source remote.
    SourceRemoteShow,
    /// Replace the bound workspace's source remote.
    SourceRemoteRebind {
        /// The new portable Git URL.
        remote: String,
        /// Report the transition without changing the registry.
        dry_run: bool,
    },
    /// Show the bound workspace's ship mode, or rebind it to `mode`.
    ShipMode(Option<ShipMode>),
    /// Declare this machine's checkout role for a workspace.
    Role {
        /// The workspace's id or name.
        workspace: String,
        /// The local role.
        role: WorkspaceCheckoutRole,
        /// The owner machine a replica follows.
        owner: Option<String>,
    },
    /// Bind, or with `rebind` replace, the bound workspace's publication
    /// repository.
    PublicationBind {
        /// The publication repository URL.
        remote: String,
        /// The lineage identifier.
        publication_id: String,
        /// The publication branch.
        branch: String,
        /// Replace an existing lineage.
        rebind: bool,
    },
    /// Show the bound workspace's publication binding.
    PublicationShow,
    /// Remove the bound workspace's publication binding.
    PublicationRemove,
    /// Remove a workspace from the registry with the caller's flow.
    Remove(HostFlow<'a>),
    /// Remove a workspace's Orbit artifacts with the caller's flow. Only a
    /// confirmed teardown is governed: without `--confirm` it only reports.
    Teardown {
        /// Whether the destructive form was asked for.
        confirmed: bool,
        /// The teardown itself.
        teardown: HostFlow<'a>,
    },
    /// Select the bound workspace's task candidates for a fleet task page.
    TaskCandidates {
        /// Each section's filter, limit and pilot-warning selection.
        sections: Vec<CandidateSection>,
        /// Select every section in one indexed batch when the index answers.
        batch: bool,
    },
    /// Read the listed rows of these tasks, with what a fleet page shows
    /// beside them.
    TaskRows(Vec<String>),
    /// The dependency statuses of these tasks, from the coordination
    /// registry every workspace shares.
    DependencyStatuses(Vec<orbit_types::task::Task>),
    /// A workspace-claim tool verb the generic tool entry admitted.
    Claim {
        /// The verb.
        verb: WorkspaceClaimVerb,
        /// The tool input.
        input: Value,
        /// The calling agent.
        agent: Option<String>,
        /// The calling model.
        model: Option<String>,
    },
}

/// One section of a fleet task selection.
pub struct CandidateSection {
    /// The section's filter.
    pub filter: TaskListFilter,
    /// The most candidates the section reads.
    pub limit: usize,
    /// Whether the section selects tasks with pilot warnings.
    pub pilot_warnings: bool,
}

/// The workspace bound to the current checkout, if it is registered.
pub struct Shown {
    /// The Orbit root the command opened.
    pub data_root: PathBuf,
    /// The registered workspace and checkout, when there is one.
    pub registered: Option<Registered>,
}

/// A registered workspace, its checkout and its seeding.
pub struct Registered {
    /// The workspace.
    pub workspace: Workspace,
    /// This machine's checkout of it.
    pub checkout: WorkspaceCheckout,
    /// Its seeding policy.
    pub seeding: Seeding,
}

/// The seeding policy a workspace resolves and the preset last applied to it.
pub struct Seeding {
    /// The resolved policies.
    pub policies: WorkspaceSeedingPolicies,
    /// The defaults the policies leave out.
    pub left_out: SeedingPolicyExclusions,
    /// The preset last applied.
    pub preset: Option<AppliedWorkspacePreset>,
}

/// What one role declaration recorded.
pub struct RoleAssignment {
    /// The workspace's id.
    pub workspace_id: String,
    /// The declared role.
    pub role: WorkspaceCheckoutRole,
    /// The workspace's owner machine.
    pub owner_machine_id: Option<String>,
}

/// The listed rows a fleet page read, with what it shows beside them.
pub struct TaskRows {
    /// Each requested task's row, in request order.
    pub rows: Vec<Result<Option<TaskRow>, OrbitError>>,
    /// Delivery attention by task id, over the rows that read.
    pub delivery_attention: Result<BTreeMap<String, Value>, OrbitError>,
    /// The branch each task's run recorded, over the rows that read.
    pub run_branches: BTreeMap<String, String>,
}

/// What the command produced, before the caller renders it.
pub enum Outcome {
    /// The host registry.
    Listed(WorkspaceRegistry),
    /// The bound workspace.
    Shown(Box<Shown>),
    /// The bound workspace's registration.
    SourceRemote(Box<Workspace>),
    /// A source-remote rebind.
    SourceRemoteRebound(WorkspaceSourceRemoteRebind),
    /// The bound workspace's ship mode.
    ShipMode {
        /// The workspace's id.
        workspace_id: String,
        /// Its resolved mode.
        mode: ShipMode,
    },
    /// A ship-mode rebind.
    ShipModeRebound(WorkspaceShipModeRebind),
    /// A role declaration.
    RoleAssigned(RoleAssignment),
    /// A publication bind or rebind.
    PublicationBound {
        /// The new binding.
        binding: Box<WorkspacePublicationBinding>,
        /// Whether it replaced a lineage.
        rebind: bool,
    },
    /// The bound workspace's publication binding.
    Publication {
        /// The workspace's id.
        workspace_id: String,
        /// Its binding, if any.
        binding: Option<Box<WorkspacePublicationBinding>>,
    },
    /// The removed publication binding.
    PublicationRemoved(Box<WorkspacePublicationBinding>),
    /// The caller's flow ran.
    Done,
    /// Each section's task candidates.
    TaskCandidates(Vec<Result<TaskCandidateKeys, OrbitError>>),
    /// The listed task rows.
    TaskRows(Box<TaskRows>),
    /// Dependency statuses by task id.
    DependencyStatuses(BTreeMap<String, TaskStatus>),
    /// A tool verb's answer.
    Answer(Value),
}

/// Why a workspace command failed.
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

/// Authorize, audit and run one workspace command.
pub fn exec(core: &Core, command: Command<'_>) -> Result<Outcome, crate::Error> {
    exec::run::<WorkspaceApp<'_>>(core, command)
}

struct WorkspaceApp<'a>(std::marker::PhantomData<&'a ()>);

impl<'a> App for WorkspaceApp<'a> {
    type Command = Command<'a>;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        let subcommand = match command {
            Command::List => "list",
            Command::Show => "show",
            Command::SourceRemoteShow => "source-remote-show",
            Command::SourceRemoteRebind { .. } => "source-remote-rebind",
            Command::ShipMode(_) => "ship-mode",
            Command::Role { .. } => "role",
            Command::PublicationBind { rebind: false, .. } => "publication-bind",
            Command::PublicationBind { rebind: true, .. } => "publication-rebind",
            Command::PublicationShow => "publication-show",
            Command::PublicationRemove => "publication-remove",
            Command::Remove(_) => "remove",
            Command::Teardown { confirmed, .. } => {
                let operation =
                    Operation::command("workspace", "teardown").target(Some("workspace"), None);
                return if *confirmed {
                    operation
                } else {
                    operation.without_command_row()
                };
            }
            Command::TaskCandidates { .. } => "task-candidates",
            Command::TaskRows(_) => "task-rows",
            Command::DependencyStatuses(_) => "dependency-statuses",
            Command::Claim { verb, input, .. } => return Operation::of(*verb, input),
        };
        Operation::command("workspace", subcommand).target(Some("workspace"), None)
    }

    fn run(core: &Core, command: Self::Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::List => Outcome::Listed(registry::list(core)?),
            Command::Show => Outcome::Shown(Box::new(registry::show(core)?)),
            Command::SourceRemoteShow => {
                Outcome::SourceRemote(Box::new(registry::source_remote(core)?))
            }
            Command::SourceRemoteRebind { remote, dry_run } => Outcome::SourceRemoteRebound(
                registry::rebind_source_remote(core, &remote, dry_run)?,
            ),
            Command::ShipMode(None) => {
                let (workspace_id, mode) = registry::ship_mode(core)?;
                Outcome::ShipMode { workspace_id, mode }
            }
            Command::ShipMode(Some(mode)) => {
                Outcome::ShipModeRebound(registry::rebind_ship_mode(core, mode)?)
            }
            Command::Role {
                workspace,
                role,
                owner,
            } => Outcome::RoleAssigned(registry::assign_role(
                core,
                &workspace,
                role,
                owner.as_deref(),
            )?),
            Command::PublicationBind {
                remote,
                publication_id,
                branch,
                rebind,
            } => Outcome::PublicationBound {
                binding: Box::new(registry::bind_publication(
                    core,
                    &remote,
                    &publication_id,
                    &branch,
                    rebind,
                )?),
                rebind,
            },
            Command::PublicationShow => {
                let (workspace_id, binding) = registry::publication(core)?;
                Outcome::Publication {
                    workspace_id,
                    binding: binding.map(Box::new),
                }
            }
            Command::PublicationRemove => {
                Outcome::PublicationRemoved(Box::new(registry::unbind_publication(core)?))
            }
            Command::Remove(flow) | Command::Teardown { teardown: flow, .. } => {
                flow(&runtime.global_root())?;
                Outcome::Done
            }
            Command::TaskCandidates { sections, batch } => {
                Outcome::TaskCandidates(tasks::candidates(core, &sections, batch)?)
            }
            Command::TaskRows(ids) => Outcome::TaskRows(Box::new(tasks::rows(core, &ids))),
            Command::DependencyStatuses(tasks) => {
                Outcome::DependencyStatuses(runtime.dependency_status_index(tasks.iter())?)
            }
            Command::Claim {
                verb,
                input,
                agent,
                model,
            } => {
                let input = verb.spec().parse_input(input)?.into_value();
                Outcome::Answer(match verb {
                    WorkspaceClaimVerb::Acquire => {
                        crate::runtime::workspace::claim::acquire(runtime, input, agent, model)?
                    }
                    WorkspaceClaimVerb::Release => {
                        crate::runtime::workspace::claim::release(runtime, input, agent, model)?
                    }
                    WorkspaceClaimVerb::Show => crate::runtime::workspace::claim::show(runtime)?,
                })
            }
        })
    }
}
