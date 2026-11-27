//! The tool app: tool registry management (`orbit tool list|show|add|remove|enable|disable|doctor`)
//! and the generic JSON tool entry shared by `orbit tool run`, plugin CLI groups and MCP.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSessionContext};
use serde_json::Value;

pub use crate::adapter::command::{
    DoctorResult, DoctorStatus, DryRunResult, ToolEntryPoint, ToolInfo,
};

use super::exec::{self, App, Operation};
use crate::Core;

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::adapter::command::tool_suggestions::unknown_tool_error;
pub use crate::adapter::command::{
    ClaimedOwnerCall, ClaimedOwnerRoute, GlobalAuditStoreCache, GlobalToolCall,
    bridge_claimed_owner_call, execute_global_in_process_tool_dispatch,
    execute_global_in_process_tool_dispatch_with_audit_writer, owner_route_unavailable,
    tool_suggestion_context,
};

const LOCAL_MACHINE_ID_FALLBACK: &str = "host/local";

/// One tool command, shared by the CLI and MCP surfaces.
pub enum Command {
    /// List active tools, or all tools including inactive ones.
    List {
        /// Include inactive tools hidden from the default agent surface.
        all: bool,
    },
    /// Show detailed metadata and input schema for one tool.
    Show(String),
    /// Register an external tool.
    Add {
        /// Resolved tool name to register.
        name: String,
        /// Explicit `--name` flag for audit attribution, when provided.
        audit_name: Option<String>,
        /// Path to the external tool executable.
        path: String,
        /// Tool description.
        description: String,
        /// Declared tool parameters.
        parameters: Vec<ToolParam>,
    },
    /// Remove an external tool.
    Remove(String),
    /// Enable a tool.
    Enable(String),
    /// Disable a tool.
    Disable(String),
    /// Diagnose registered tool health.
    Doctor {
        /// Optional tool name filter, recorded as the audit target.
        name: Option<String>,
    },
    /// Preview CLI tool admission and missing required parameters without executing.
    DryRun {
        /// Tool name.
        name: String,
        /// Prepared JSON input.
        input: Value,
        /// Explicit agent override.
        agent: Option<String>,
        /// Explicit model override.
        model: Option<String>,
        /// Session context for admission.
        session: ToolSessionContext,
    },
    /// Execute a tool through the generic audited tool entry.
    Run {
        /// Tool name.
        name: String,
        /// Tool JSON input.
        input: Value,
        /// Explicit agent override.
        agent: Option<String>,
        /// Explicit model override.
        model: Option<String>,
        /// Entry point (CLI or MCP).
        entry_point: ToolEntryPoint,
        /// Session context.
        session: ToolSessionContext,
    },
    /// Audit an in-process refusal through the tool dispatch audit boundary.
    AuditRefusal {
        /// Tool name.
        name: String,
        /// Tool JSON input.
        input: Value,
        /// Entry point (CLI or MCP).
        entry_point: ToolEntryPoint,
        /// Session context.
        session: ToolSessionContext,
        /// The refusal error to record and return.
        error: OrbitError,
    },
    /// Read the workspace roots, identity and claim state needed to bind a CLI tool call.
    CliWorkspaceBinding,
}

/// Workspace and identity facts read from a [`Core`] for CLI tool binding and session context.
pub struct CliWorkspaceBinding {
    /// Host global Orbit data root.
    pub global_root: PathBuf,
    /// Bound checkout repository root.
    pub repo_root: PathBuf,
    /// Bound workspace task-partition id, when available.
    pub workspace_id: Result<String, OrbitError>,
    /// Local machine identity (or the historical unattributed fallback).
    pub machine_id: String,
    /// Local machine name, when configured.
    pub machine_name: Option<String>,
    /// Whether this process is a claimed worker routed to a remote owner.
    pub is_remote_claimed_owner: bool,
    /// Registered `(id, name)` of the bound checkout, if registered.
    pub registered_workspace: Option<(String, String)>,
    /// Composition snapshot attached when this workspace was opened, if any.
    pub composition_snapshot: Option<Arc<dyn Any + Send + Sync>>,
}

/// What a tool command produced.
pub enum Outcome {
    /// Listed tools.
    Listed(Vec<ToolInfo>),
    /// One tool's metadata and schema.
    Shown(Box<ToolInfo>),
    /// Tool health diagnostics.
    Diagnosed(Vec<DoctorResult>),
    /// Dry-run admission preview.
    DryRun(DryRunResult),
    /// JSON value returned by a tool call.
    Executed(Value),
    /// Workspace binding facts for CLI tool calls.
    CliWorkspaceBinding(Box<CliWorkspaceBinding>),
    /// A write with no payload.
    Done,
}

impl Outcome {
    /// Extract the listed tools.
    pub fn into_listed(self) -> Result<Vec<ToolInfo>, crate::Error> {
        match self {
            Self::Listed(tools) => Ok(tools),
            _ => Err(unexpected_outcome("Listed")),
        }
    }

    /// Extract one tool's metadata.
    pub fn into_shown(self) -> Result<ToolInfo, crate::Error> {
        match self {
            Self::Shown(tool) => Ok(*tool),
            _ => Err(unexpected_outcome("Shown")),
        }
    }

    /// Extract the doctor diagnostics.
    pub fn into_diagnosed(self) -> Result<Vec<DoctorResult>, crate::Error> {
        match self {
            Self::Diagnosed(results) => Ok(results),
            _ => Err(unexpected_outcome("Diagnosed")),
        }
    }

    /// Extract the dry-run preview.
    pub fn into_dry_run(self) -> Result<DryRunResult, crate::Error> {
        match self {
            Self::DryRun(result) => Ok(result),
            _ => Err(unexpected_outcome("DryRun")),
        }
    }

    /// Extract the executed tool's JSON value.
    pub fn into_value(self) -> Result<Value, crate::Error> {
        match self {
            Self::Executed(value) => Ok(value),
            _ => Err(unexpected_outcome("Executed")),
        }
    }

    /// Extract the CLI workspace binding facts.
    pub fn into_cli_workspace_binding(self) -> Result<CliWorkspaceBinding, crate::Error> {
        match self {
            Self::CliWorkspaceBinding(binding) => Ok(*binding),
            _ => Err(unexpected_outcome("CliWorkspaceBinding")),
        }
    }
}

/// Why a tool command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The underlying tool operation failed.
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

struct ToolApp;

impl App for ToolApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            Command::List { .. } => Operation::command("tool", "list")
                .audit_role("admin")
                .without_command_row(),
            Command::Show(name) => Operation::command("tool", "show")
                .target(Some("tool"), Some(name.clone()))
                .recorded_tool_name(Some(name.clone()))
                .audit_role("admin")
                .without_command_row(),
            Command::Add { audit_name, .. } => Operation::command("tool", "add")
                .target(Some("tool"), audit_name.clone())
                .recorded_tool_name(audit_name.clone())
                .audit_role("admin")
                .without_command_row(),
            Command::Remove(name) => Operation::command("tool", "remove")
                .target(Some("tool"), Some(name.clone()))
                .recorded_tool_name(Some(name.clone()))
                .audit_role("admin")
                .without_command_row(),
            Command::Enable(name) => Operation::command("tool", "enable")
                .target(Some("tool"), Some(name.clone()))
                .recorded_tool_name(Some(name.clone()))
                .audit_role("admin")
                .without_command_row(),
            Command::Disable(name) => Operation::command("tool", "disable")
                .target(Some("tool"), Some(name.clone()))
                .recorded_tool_name(Some(name.clone()))
                .audit_role("admin")
                .without_command_row(),
            Command::Doctor { name } => Operation::command("tool", "doctor")
                .target(Some("tool"), name.clone())
                .recorded_tool_name(name.clone())
                .audit_role("admin")
                .without_command_row(),
            Command::DryRun {
                name,
                input,
                agent,
                model,
                ..
            } => Operation::command("tool", "run")
                .target(Some("tool"), Some(name.clone()))
                .recorded_tool_name(Some(name.clone()))
                .tool_run_audit_role(input.clone(), agent.clone(), model.clone())
                .without_command_row(),
            // The generic tool dispatch owns its own authorization and audit row.
            Command::Run { .. } | Command::AuditRefusal { .. } => Operation::command("tool", "run")
                .without_command_row()
                .without_invocation_audit(),
            Command::CliWorkspaceBinding => {
                Operation::dashboard_read("tool", "cli-workspace-binding")
            }
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        let outcome = match command {
            Command::List { all } => Outcome::Listed(if all {
                runtime.list_all_tools()?
            } else {
                runtime.list_tools()?
            }),
            Command::Show(name) => Outcome::Shown(Box::new(runtime.show_tool(&name)?)),
            Command::Add {
                name,
                audit_name: _,
                path,
                description,
                parameters,
            } => {
                runtime.add_tool(&name, &path, &description, parameters)?;
                Outcome::Done
            }
            Command::Remove(name) => {
                runtime.remove_tool(&name)?;
                Outcome::Done
            }
            Command::Enable(name) => {
                runtime.enable_tool(&name)?;
                Outcome::Done
            }
            Command::Disable(name) => {
                runtime.disable_tool(&name)?;
                Outcome::Done
            }
            Command::Doctor { .. } => Outcome::Diagnosed(runtime.doctor()?),
            Command::DryRun {
                name,
                input,
                agent: _,
                model: _,
                session,
            } => Outcome::DryRun(
                runtime.run_tool_dry_run_with_session_context(&name, &input, session)?,
            ),
            Command::Run {
                name,
                input,
                agent,
                model,
                entry_point,
                session,
            } => {
                let output = runtime
                    .execute_tool_command_dispatch_with_session_context(
                        &name,
                        input,
                        agent,
                        model,
                        entry_point,
                        session,
                    )?
                    .value;
                Outcome::Executed(output)
            }
            Command::AuditRefusal {
                name,
                input,
                entry_point,
                session,
                error,
            } => {
                let output = runtime
                    .execute_in_process_tool_dispatch(
                        &name,
                        input,
                        entry_point,
                        session,
                        move |_| Err(error),
                    )?
                    .value;
                Outcome::Executed(output)
            }
            Command::CliWorkspaceBinding => {
                Outcome::CliWorkspaceBinding(Box::new(cli_workspace_binding(core)))
            }
        };
        Ok(outcome)
    }
}

/// Run one tool command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<ToolApp>(core, command)
}

fn cli_workspace_binding(core: &Core) -> CliWorkspaceBinding {
    let runtime = core.runtime();
    let (machine_id, machine_name) = match runtime.automation_execution_location() {
        Some(location) => (location.machine_id.clone(), location.machine_name.clone()),
        None => (LOCAL_MACHINE_ID_FALLBACK.to_string(), None),
    };
    let registered_workspace = match runtime.opened_registered_workspace() {
        Some(opened) => opened.map(|(id, name)| (id.to_owned(), name.to_owned())),
        None => orbit_registry::workspace_registry::load_registry_from(
            &orbit_registry::workspace_registry::registry_path_for(&runtime.global_root()),
        )
        .ok()
        .and_then(|registry| {
            orbit_registry::workspace_registry::find_workspace_by_path(
                &registry,
                &runtime.paths().repo_root,
            )
            .map(|workspace| (workspace.id.clone(), workspace.name.clone()))
        }),
    };
    CliWorkspaceBinding {
        global_root: runtime.global_root(),
        repo_root: runtime.paths().repo_root.clone(),
        workspace_id: runtime.workspace_id(),
        machine_id,
        machine_name,
        is_remote_claimed_owner: runtime
            .run()
            .claim_owner()
            .is_some_and(|owner| owner.is_remote()),
        registered_workspace,
        composition_snapshot: runtime.composition_snapshot_arc(),
    }
}

fn unexpected_outcome(expected: &str) -> crate::Error {
    OrbitError::Execution(format!(
        "tool command answered an outcome other than {expected}"
    ))
}
