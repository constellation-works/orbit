//! `orbit run ship` CLI entrypoint.

use clap::{Args, ValueEnum};
use orbit_core::application::job::SubmissionKind;
use orbit_core::{CompletionPolicy, OrbitError, OrbitRuntime, find_workflow};

use crate::command::{CommandOut, Execute};

use super::support::{WorkflowDispatchResult, workflow_dispatch_payload_with_notices};

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
    after_help = "Examples:\n  orbit run ship\n  orbit run ship T123\n  orbit run ship T123 T456 --mode local\n  orbit run ship T123 --base main\n  orbit run ship T123 --allow-crew sol\n  orbit run ship T123 --complete\n  orbit run ship T123 --review before-landing\n\n\
                  Shipment is asynchronous: prints the run ID and returns before\n\
                  the work completes. Follow it with `orbit run show <RUN_ID>`.\n\
                  Details: the orbit-orchestrate skill, references/orchestration.md."
)]
pub struct ShipCommand {
    /// Optional task IDs to seed explicit gated shipment. Omit for auto mode.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Pipeline mode. Explicit selections ship as singletons
    /// (default: registry `ship_mode`, else `pr`).
    #[arg(short = 'm', long, value_enum)]
    pub mode: Option<ShipMode>,
    /// Base branch for shipment (default: registered workspace base branch,
    /// else `[workflow] base_branch`, else `main`).
    #[arg(short = 'b', long)]
    pub base: Option<String>,
    /// Authorize this run to finish delivery and move shipped tasks to `done`
    /// instead of `review`. Default off; never approves `proposed` work.
    #[arg(long)]
    pub complete: bool,
    /// Restrict an explicit shipment to these configured crews
    /// (repeatable, comma-separated).
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Add a review layer before landing to this run when workspace policy
    /// applies none. Only adds review. `before-landing` is recommended;
    /// `before-pr` is deprecated but still holds PR creation for a reviewer.
    #[arg(long, value_enum, value_name = "LAYER")]
    pub review: Option<orbit_types::workflow::ReviewLayer>,
    /// Require a systemd user scope for this run and every worker it starts.
    /// Treats machine.worker_containment as strict for this invocation.
    #[arg(long)]
    pub strict_worker_containment: bool,
    /// Start the named tasks now even if their scheduled start (`not_before`)
    /// is still ahead. Records a `schedule_overridden` history event.
    #[arg(long, requires = "task_ids")]
    pub ignore_schedule: bool,
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
            self.review,
            self.ignore_schedule,
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
            wait_timeout: false,
            error_code: None,
            error_message: None,
        };
        let notices = runtime.submission_notices(SubmissionKind::Ship {
            explicit: !self.task_ids.is_empty(),
        });
        workflow_dispatch_payload_with_notices(
            SHIP_WORKFLOW,
            &[run],
            notices.warnings(),
            notices.notes(),
        )
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

fn ensure_workflow_exists(workflow_alias: &'static str) -> Result<(), OrbitError> {
    find_workflow(workflow_alias)
        .map(|_| ())
        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown workflow '{workflow_alias}'")))
}
