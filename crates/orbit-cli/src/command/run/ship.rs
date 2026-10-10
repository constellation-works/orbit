//! `orbit run ship` CLI entrypoint.

use clap::{Args, ValueEnum};
use orbit_core::{CompletionPolicy, OrbitError, OrbitRuntime, find_workflow};

use crate::command::{CommandOut, Execute};

use super::support::{WorkflowDispatchResult, workflow_dispatch_payload_with_warning};

const SHIP_WORKFLOW: &str = "ship";

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
    /// Base branch for shipment. Defaults to the registered workspace
    /// base branch, else `[workflow] base_branch` from `config.toml`
    /// (or `main` if unset).
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
    /// Require a systemd user scope for this run and every worker it starts.
    /// Overrides machine.worker_containment_strict for this invocation.
    #[arg(long)]
    pub strict_worker_containment: bool,
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
        let invoke = runtime.submit_ship_run_with_containment(
            mode,
            self.base.as_deref(),
            &self.task_ids,
            self.completion(),
            &self.allow_crew,
            None,
            self.claim_token.as_deref(),
            orbit_types::workflow::JobRunTrigger::cli(),
            self.strict_worker_containment,
        )?;
        super::support::warn_unset_env_pass(runtime);
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
            wait_timeout: false,
            error_code: None,
            error_message: None,
        };
        // [ORB-13901] Discovery is refused while throttled; an explicit
        // selection proceeds and is warned.
        let mut warnings: Vec<String> = if self.task_ids.is_empty() {
            Vec::new()
        } else {
            super::auto::resource_throttle_warning(runtime)
                .into_iter()
                .collect()
        };
        warnings.extend(runtime.validation_env_preflight_warning());
        workflow_dispatch_payload_with_warning(SHIP_WORKFLOW, &[run], warnings)
    }
}

/// Resolve the effective ship mode for a `ship` invocation.
///
/// An explicit `--mode` wins. Otherwise the mode is resolved from the current
/// workspace binding. Standalone runtimes without a binding may use a single
/// unambiguous registry checkout for their data root. If no workspace can be
/// identified, fall back to `pr` so omitted configuration uses reviewable delivery.
fn resolve_ship_mode(
    args: &ShipCommand,
    runtime: &OrbitRuntime,
) -> Result<orbit_core::ShipMode, OrbitError> {
    if let Some(mode) = args.mode {
        return Ok(mode.to_core());
    }
    if let Some(binding) = runtime.workspace_runtime_binding() {
        return Ok(binding.ship_mode);
    }
    let registry_path =
        orbit_registry::workspace_registry::registry_path_for(&runtime.global_root());
    let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
    let orbit_dir = runtime.shared_root();
    let mut checkouts = registry
        .checkouts
        .iter()
        .filter(|checkout| checkout.orbit_dir == orbit_dir);
    let checkout = checkouts.next().filter(|_| checkouts.next().is_none());
    let mode = checkout
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
}

impl Execute for LegacyShipLocalCommand {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        let _ = self;
        Err(OrbitError::InvalidInput(
            "`orbit run ship-local` was replaced by `orbit run ship --mode local`".to_string(),
        ))
    }
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
