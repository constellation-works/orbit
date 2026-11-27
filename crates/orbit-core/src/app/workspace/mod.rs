//! The workspace app: the host workspace registry commands of
//! `orbit workspace`, the dashboard's fleet task reads, and the
//! workspace-claim tool verbs.
//!
//! A registry command reads or writes the host workspace registry for the
//! workspace this `Core` is bound to. Removal and teardown are run by the
//! caller's own flow, which the app runs once it has admitted the command:
//! that flow lives above this crate.

mod live_run;

mod migrate;

mod preset;

mod product_profile;

mod registry;

mod sync;

mod tasks;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
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

pub use self::migrate::MigrateStatus;

use super::exec::{self, App, Operation};
use crate::app::agent::{
    ClaimedEffortBackfill, DispatchEffortBackfill, backfill_claimed_leaf_effort,
    backfill_dispatch_effort,
};
use crate::application::codebase::DiffStatsBackfill;
use crate::application::job::TaskCrewBackfill;
use crate::application::seeding_policy::SeedingPolicyExclusions;
use crate::application::task::{TaskListFilter, TaskRow};
use crate::{Core, ShipMode};
use preset::AppliedWorkspacePreset;

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::adapter::HubCoordinationExecutor;
pub use crate::application::workspace_auto::FleetReadinessFacts;
pub use crate::bootstrap::init::{
    InitOptions, allocated_task_prefix, init_global, init_workspace_at_root,
};
pub use crate::bootstrap::task_migration::seed_task_id_start;
pub use crate::composition::{
    Opening, WorkspaceAuthorityAdoption, adopt_workspace_layout_authority, core_for_request,
    store_opener, verify_workspace_layout_authority,
};
pub use live_run::workspace_live_run_ids;
pub use orbit_runtime::{
    WorkspaceRuntimeBinding, is_global_orbit_root, managed_workspace_selector_from_env,
    resolve_generation_root, resolve_global_root, resolve_process_generation_root,
    resolve_root_path_value,
};
pub use preset::WorkspacePreset;
pub(crate) use product_profile::ProductProfile;
pub(crate) use sync::reconcile_workspace_catalog_copies;
pub use sync::{
    ManagedArtifactOutcome, ManagedArtifactScope, ManagedArtifactSyncAction,
    WorkspaceManagedArtifactSyncReport, reconcile_workspace_managed_artifacts,
    seed_absent_workspace_managed_artifacts,
};

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
    /// Aggregate task pages across the bound workspace and its peers:
    /// select candidates concurrently, merge in workspace order, hydrate
    /// surviving rows and read the shared dependency status index.
    AggregateTasks(AggregateTaskRequest<'a>),
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
    /// Report layout and schema migration status after applying migrations on open.
    MigrateStatus,
    /// Backfill landed diff stats for done tasks.
    BackfillDiffStats(usize),
    /// Backfill task crews from latest delivery selections.
    BackfillTaskCrews,
    /// Backfill dispatch reasoning effort from audited launches.
    BackfillDispatchEffort,
    /// Backfill claimed leaf effort and model reports to the owner.
    BackfillClaimedEffort,
}

/// Inputs for one aggregate task listing across registered workspaces.
pub struct AggregateTaskRequest<'a> {
    /// Workspaces in response order, each with its opened entry or open failure.
    pub workspaces: Vec<AggregateWorkspace<'a>>,
    /// Sections to read: one for an ungrouped page, or one per requested status.
    pub sections: Vec<AggregateTaskSection>,
    /// Index in `workspaces` where a continuation cursor resumes; earlier
    /// workspaces contribute only to `total`.
    pub resume_index: usize,
}

/// One workspace slot in an aggregate task read, in response order.
pub struct AggregateWorkspace<'a> {
    /// The workspace's registered id, used for diagnostics and reporting.
    pub id: &'a str,
    /// How this workspace participates in the aggregate read.
    pub source: AggregateWorkspaceSource<'a>,
}

/// How one workspace participates in an aggregate task read.
pub enum AggregateWorkspaceSource<'a> {
    /// The bound [`Core`] passed to [`exec`].
    Bound,
    /// Another opened workspace's [`Core`].
    Peer(&'a Core),
    /// A workspace that was inactive or could not be opened.
    Unavailable(String),
}

/// One section of an aggregate task read.
pub struct AggregateTaskSection {
    /// The section's filter, including `scan_before` for the resume workspace.
    pub filter: TaskListFilter,
    /// The most rows this section returns.
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

/// Hydrated sections returned by an aggregate task listing.
pub struct AggregateTaskPages {
    /// One page per requested section, in request order.
    pub sections: Vec<AggregateTaskPage>,
}

/// One hydrated page of an aggregate task listing.
pub struct AggregateTaskPage {
    /// Hydrated rows in workspace-then-recency order.
    pub rows: Vec<AggregateTaskRow>,
    /// Pre-limit, pre-cursor matching count across answering workspaces.
    pub total: usize,
    /// Number of candidates selected before row hydration (for cursor offset).
    pub candidate_count: usize,
    /// Continuation anchor when more matching candidates remain after this page.
    pub next_cursor: Option<AggregateCursorAnchor>,
    /// Delivery attention per task id, keyed by workspace index.
    pub delivery_attention: BTreeMap<usize, BTreeMap<String, Value>>,
    /// Local run branches per task id, keyed by workspace index.
    pub run_branches: BTreeMap<usize, BTreeMap<String, String>>,
    /// Dependency statuses for this section's rows from the shared registry.
    pub dependency_statuses: BTreeMap<String, TaskStatus>,
    /// Workspaces that could not be opened or read for this section, once each.
    pub unavailable: Vec<UnavailableWorkspace>,
}

/// One hydrated task row in page order, paired with its workspace index.
pub struct AggregateTaskRow {
    /// Index into [`AggregateTaskRequest::workspaces`].
    pub workspace_index: usize,
    /// The hydrated task row.
    pub row: TaskRow,
}

/// Where the next aggregate page resumes when more candidates remain.
pub struct AggregateCursorAnchor {
    /// Index into [`AggregateTaskRequest::workspaces`] of the last candidate.
    pub workspace_index: usize,
    /// Creation time of the last candidate.
    pub created_at: DateTime<Utc>,
    /// Task id of the last candidate.
    pub task_id: String,
}

/// One workspace an aggregate section could not read completely.
#[derive(Clone)]
pub struct UnavailableWorkspace {
    /// Index into [`AggregateTaskRequest::workspaces`].
    pub workspace_index: usize,
    /// Why the workspace could not be opened or read.
    pub error: String,
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
    /// Hydrated sections of an aggregate task listing.
    AggregateTasks(AggregateTaskPages),
    /// A tool verb's answer.
    Answer(Value),
    /// Layout and schema migration status after open.
    MigrateStatus(Box<MigrateStatus>),
    /// Diff stats backfill report.
    DiffStatsBackfill(DiffStatsBackfill),
    /// Task crew backfill report.
    TaskCrewBackfill(TaskCrewBackfill),
    /// Dispatch effort backfill report.
    DispatchEffortBackfill(DispatchEffortBackfill),
    /// Claimed leaf effort backfill report.
    ClaimedEffortBackfill(ClaimedEffortBackfill),
}

impl Outcome {
    /// Extract the aggregate task pages answered by [`Command::AggregateTasks`].
    pub fn into_aggregate_tasks(self) -> Result<AggregateTaskPages, crate::Error> {
        match self {
            Self::AggregateTasks(pages) => Ok(pages),
            _ => Err(OrbitError::Execution(
                "workspace command answered an outcome other than AggregateTasks".to_string(),
            )),
        }
    }
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
            Command::AggregateTasks(_) => "aggregate-tasks",
            Command::Claim { verb, input, .. } => return Operation::of(*verb, input),
            Command::MigrateStatus
            | Command::BackfillDiffStats(_)
            | Command::BackfillTaskCrews
            | Command::BackfillDispatchEffort
            | Command::BackfillClaimedEffort => {
                return Operation::command("migrate", "")
                    .without_recorded_subcommand()
                    .target(Some("workspace"), None)
                    .audit_role("admin")
                    .without_command_row();
            }
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
            Command::AggregateTasks(request) => {
                Outcome::AggregateTasks(tasks::aggregate(core, &request)?)
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
            Command::MigrateStatus => Outcome::MigrateStatus(Box::new(migrate::status(core)?)),
            Command::BackfillDiffStats(limit) => {
                Outcome::DiffStatsBackfill(runtime.backfill_landed_diff_stats(limit)?)
            }
            Command::BackfillTaskCrews => {
                Outcome::TaskCrewBackfill(runtime.backfill_admitted_task_crews()?)
            }
            Command::BackfillDispatchEffort => {
                Outcome::DispatchEffortBackfill(backfill_dispatch_effort(runtime)?)
            }
            Command::BackfillClaimedEffort => {
                Outcome::ClaimedEffortBackfill(backfill_claimed_leaf_effort(runtime)?)
            }
        })
    }
}

/// Enable the plugin-seeded routines and auto-tasks `preset` names in the
/// workspace `core` opened, for `orbit workspace init --preset`, which applies
/// the preset to the checkout it has just registered. Returns the names whose
/// state changed.
pub fn enable_preset_definitions(
    core: &Core,
    preset: &WorkspacePreset,
) -> Result<Vec<String>, OrbitError> {
    preset::enable_preset_definitions(core.runtime(), preset)
}

/// Authorize, audit and run one workspace command.
pub fn exec(core: &Core, command: Command<'_>) -> Result<Outcome, crate::Error> {
    exec::run::<WorkspaceApp<'_>>(core, command)
}
