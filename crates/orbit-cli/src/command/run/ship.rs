//! `orbit run ship` CLI entrypoint.

use clap::{Args, ValueEnum};
#[cfg(test)]
use orbit_core::build_ship_input;
use orbit_core::{CompletionPolicy, OrbitError, OrbitRuntime, find_workflow};
#[cfg(test)]
use serde_json::Value;

use crate::command::{CommandOut, Execute};

use super::support::{WorkflowDispatchResult, workflow_dispatch_payload};

pub(super) const SHIP_WORKFLOW: &str = "ship";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ShipMode {
    Pr,
    Local,
}

impl ShipMode {
    pub(super) fn to_core(self) -> orbit_core::ShipMode {
        match self {
            ShipMode::Pr => orbit_core::ShipMode::Pr,
            ShipMode::Local => orbit_core::ShipMode::Local,
        }
    }
}

#[derive(Args)]
#[command(
    about = "Ship backlog or explicitly selected tasks through the gated task pipeline",
    override_usage = "orbit run ship [<TASK_ID>...] [OPTIONS]",
    after_help = "Examples:\n  orbit run ship\n  orbit run ship T123\n  orbit run ship T123 T456 --mode local\n  orbit run ship T123 --base main\n  orbit run ship T123 --allow-crew sol\n  orbit run ship T123 --complete\n\n\
                  Shipment is asynchronous: this prints the durable run ID and returns. The\n\
                  eventual outcome is not known when it does.\n\n\
                  `--allow-crew` restricts an explicit shipment to configured crews. It does\n\
                  not reassign tasks or select a fallback: an excluded current or later-resolved\n\
                  crew is refused before its provider starts.\n\n\
                  Inspect submitted runs with `orbit run history -j task_auto_pipeline` and\n\
                  `orbit run show <RUN_ID>`."
)]
pub struct ShipCommand {
    /// Optional task IDs to seed explicit gated shipment. Omit for auto mode.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Pipeline mode for selected or auto-discovered task bundles. When omitted,
    /// the mode is resolved from the current workspace's registry entry
    /// (explicit `ship_mode`, else defaults to `pr`).
    #[arg(short = 'm', long, value_enum)]
    pub mode: Option<ShipMode>,
    /// Base branch for shipment. Defaults to
    /// `[workflow] base_branch` from `config.toml` (or `main` if unset).
    #[arg(short = 'b', long)]
    pub base: Option<String>,
    /// Authorize this run to finish delivery and move the tasks it ships to
    /// `done`, instead of leaving them in `review` for a separate approval.
    /// In `local` mode that happens once the work is merged and pushed; in
    /// `pr` mode once the PR is verified merged, respecting branch protections
    /// and required checks. Off by default, and it never approves `proposed`
    /// work for the backlog.
    #[arg(long)]
    pub complete: bool,
    /// Restrict an explicit shipment to these configured crews. Repeatable and
    /// comma-separated. Every name must be configured; an unknown or empty one
    /// fails before a run is created. Omitted, shipment remains unrestricted.
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
    /// Token for this workspace's exclusive claim, when another operator holds
    /// one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`.
    #[arg(long)]
    pub claim_token: Option<String>,
}

impl ShipCommand {
    fn completion(&self) -> CompletionPolicy {
        if self.complete {
            CompletionPolicy::Done
        } else {
            CompletionPolicy::Review
        }
    }
}

impl Execute for ShipCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mode = resolve_ship_mode(&self, runtime)?;
        validate_task_selection(&self.task_ids)?;
        ensure_workflow_exists(SHIP_WORKFLOW)?;
        // Ship is the one workflow whose submission carries task-level
        // admission checks, so it must not use the generic CLI dispatcher.
        let invoke = runtime.submit_ship_run(
            mode,
            self.base.as_deref(),
            &self.task_ids,
            self.completion(),
            &self.allow_crew,
            None,
            self.claim_token.as_deref(),
        )?;
        let run = WorkflowDispatchResult {
            workflow_alias: SHIP_WORKFLOW,
            job_id: invoke.job_name,
            run_id: invoke.run_id,
            state: if invoke.queued {
                "queued".to_string()
            } else {
                "submitted".to_string()
            },
            attempt: 1,
            error_code: None,
            error_message: None,
        };
        workflow_dispatch_payload(SHIP_WORKFLOW, &[run])
    }
}

/// Resolve the effective ship mode for a `ship` invocation.
///
/// An explicit `--mode` wins. Otherwise the mode is resolved from the current
/// workspace's registry entry (matched by `orbit_dir`): explicit `ship_mode`,
/// else the `pr` default. If the current workspace isn't found in the registry,
/// fall back to `pr` so omitted configuration still uses reviewable delivery.
pub(crate) fn resolve_ship_mode(
    args: &ShipCommand,
    runtime: &OrbitRuntime,
) -> Result<orbit_core::ShipMode, OrbitError> {
    if let Some(mode) = args.mode {
        return Ok(mode.to_core());
    }
    let registry_path =
        orbit_registry::workspace_registry::registry_path_for(&runtime.global_root());
    let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
    let orbit_dir = runtime.shared_root();
    let mode = registry
        .checkouts
        .iter()
        .find(|checkout| checkout.orbit_dir == orbit_dir)
        .and_then(|checkout| {
            registry
                .workspaces
                .iter()
                .find(|workspace| workspace.id == checkout.workspace_id)
        })
        .map(orbit_core::resolved_ship_mode);
    let Some(mode) = mode else {
        tracing::warn!(
            registry_path = %registry_path.display(),
            orbit_dir = %orbit_dir.display(),
            fallback = "pr",
            "workspace was not found in registry; falling back to PR ship mode"
        );
        return Ok(orbit_core::ShipMode::Pr);
    };
    Ok(mode)
}

#[derive(Args)]
#[command(
    about = "Deprecated alias for `orbit run ship --mode local`",
    override_usage = "orbit run ship-local [<TASK_ID>...] [OPTIONS]",
    after_help = "`orbit run ship-local` was replaced by `orbit run ship --mode local`."
)]
pub struct LegacyShipLocalCommand {
    /// Deprecated. Pass task IDs to `orbit run ship --mode local`.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Deprecated. Use `orbit run ship --mode local --base <BRANCH>`.
    #[arg(short = 'b', long)]
    pub base: Option<String>,
    /// Deprecated.
    #[arg(long)]
    pub json: bool,
}

impl Execute for LegacyShipLocalCommand {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        let _ = self;
        Err(OrbitError::InvalidInput(
            "`orbit run ship-local` was replaced by `orbit run ship --mode local`".to_string(),
        ))
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct WorkflowRunPlan {
    pub workflow_alias: &'static str,
    pub input: Value,
}

#[cfg(test)]
pub(crate) fn build_ship_run_plan(
    args: &ShipCommand,
    config_base_branch: &str,
    mode: orbit_core::ShipMode,
) -> Result<WorkflowRunPlan, OrbitError> {
    validate_task_selection(&args.task_ids)?;
    let workflow_alias = SHIP_WORKFLOW;
    ensure_workflow_exists(workflow_alias)?;
    let base = args.base.as_deref().unwrap_or(config_base_branch);
    Ok(WorkflowRunPlan {
        workflow_alias,
        input: build_ship_input(
            mode,
            base,
            &args.task_ids,
            args.completion(),
            &args.allow_crew,
        )?,
    })
}

fn validate_task_selection(task_ids: &[String]) -> Result<(), OrbitError> {
    if let Some(legacy) = task_ids.first().and_then(|value| legacy_ship_form(value)) {
        return Err(OrbitError::InvalidInput(legacy.to_string()));
    }
    Ok(())
}

fn legacy_ship_form(value: &str) -> Option<&'static str> {
    match value {
        "local" => {
            Some("`orbit run ship local` was replaced by `orbit run ship --mode local <TASK_ID>`")
        }
        "pr" => Some("`orbit run ship pr` was replaced by `orbit run ship --mode pr <TASK_ID>`"),
        "auto" | "ship-auto" => Some(
            "`orbit run ship auto` was replaced by `orbit run auto`; `orbit run ship` remains leaf-only auto shipment when no task ids are supplied",
        ),
        "list" | "show" => Some(
            "`orbit run ship list/show` was removed; use `orbit run history -j <JOB_ID>` and `orbit run show <RUN_ID>` for run inspection",
        ),
        _ => None,
    }
}

fn ensure_workflow_exists(workflow_alias: &'static str) -> Result<(), OrbitError> {
    find_workflow(workflow_alias)
        .map(|_| ())
        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown workflow '{workflow_alias}'")))
}
