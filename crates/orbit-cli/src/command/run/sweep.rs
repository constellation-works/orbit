//! `orbit run ship-sweep` — dispatch ship runs across every registered
//! workspace that opted in (`[workflow] auto_ship = true`) and has ready
//! backlog work. Designed for unattended schedulers (systemd timer, cron):
//! it never bootstraps a workspace from the caller's cwd, isolates
//! per-workspace failures, and exits non-zero only when a workspace errored.

use std::path::Path;

use clap::Args;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::{
    JobRunState, OrbitError, TaskReferenceIndex, TaskStatus, task_dependencies_ready_with_index,
};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};
use serde_json::{Value, json};

use super::ship::ShipMode;
use super::support::TASK_AUTO_PIPELINE_JOB;
use crate::command::{CommandOut, Payload};

#[derive(Args)]
#[command(
    name = "ship-sweep",
    about = "Dispatch ship runs in every registered workspace with ready backlog tasks",
    after_help = "Only workspaces with `[workflow] auto_ship = true` in their config.toml are\n\
                  swept; everything else is reported as skipped. Intended to run from a\n\
                  scheduler (systemd timer / cron), e.g.:\n  orbit run ship-sweep --json\n\n\
                  Inspect dispatched runs per workspace with `orbit run history -j task_auto_pipeline`."
)]
pub struct ShipSweepCommand {
    /// Override the pipeline mode for every dispatched ship run. When omitted,
    /// each workspace's mode is resolved from its registry entry
    /// (explicit `ship_mode`, else defaults to `pr`).
    #[arg(short = 'm', long, value_enum)]
    pub mode: Option<ShipMode>,
    /// Report what would be dispatched without submitting any run.
    #[arg(long)]
    pub dry_run: bool,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

struct SweepReport {
    workspace_id: String,
    workspace_name: String,
    action: &'static str,
    reason: Option<String>,
    ready_backlog: usize,
    /// Resolved ship mode for this workspace (`pr` / `local`). Set for the
    /// dispatch paths (`would_dispatch` / `dispatched`) so the operator can
    /// confirm per-workspace mode resolution; `None` for skips/errors.
    mode: Option<&'static str>,
    run_id: Option<String>,
    run_state: Option<&'static str>,
}

impl SweepReport {
    fn skipped(ws: &Workspace, reason: &str, ready_backlog: usize) -> Self {
        Self {
            workspace_id: ws.id.clone(),
            workspace_name: ws.name.clone(),
            action: "skipped",
            reason: Some(reason.to_string()),
            ready_backlog,
            mode: None,
            run_id: None,
            run_state: None,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "workspace_id": self.workspace_id,
            "workspace_name": self.workspace_name,
            "action": self.action,
            "reason": self.reason,
            "ready_backlog": self.ready_backlog,
            "mode": self.mode,
            "run_id": self.run_id,
            "run_state": self.run_state,
        })
    }

    fn to_line(&self) -> String {
        let mut line = format!(
            "{}: {} (ready backlog: {})",
            self.workspace_name, self.action, self.ready_backlog
        );
        if let Some(mode) = self.mode {
            line.push_str(&format!(" [{mode}]"));
        }
        if let Some(reason) = &self.reason {
            line.push_str(&format!(" — {reason}"));
        }
        if let Some(run_id) = &self.run_id {
            line.push_str(&format!(" — run {run_id}"));
            if let Some(state) = self.run_state {
                line.push_str(&format!(" [{state}]"));
            }
        }
        line
    }
}

impl ShipSweepCommand {
    /// Runs without a pre-initialized runtime: the sweep resolves every
    /// workspace from the global registry and must never bootstrap a
    /// `.orbit/` in the scheduler's working directory.
    pub fn execute_without_runtime(self) -> CommandOut {
        let global_root = workspace_registry::global_orbit_dir()?;
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let registry = workspace_registry::with_registry_lock(&registry_path, || {
            let mut registry = workspace_registry::load_registry_from(&registry_path)?;
            workspace_registry::validate_workspaces(&mut registry);
            workspace_registry::save_registry_to(&registry, &registry_path)?;
            Ok(registry)
        })?;

        let mode_override = self.mode.map(ShipMode::to_core);
        let reports: Vec<SweepReport> = workspace_registry::local_workspaces(&registry)
            .map(|(workspace, checkout)| {
                sweep_workspace(
                    &global_root,
                    workspace,
                    checkout,
                    mode_override,
                    self.dry_run,
                )
            })
            .collect();

        let failed = reports.iter().filter(|r| r.action == "error").count();
        let doc = json!({
            "dry_run": self.dry_run,
            "workspaces": reports.len(),
            "dispatched": reports.iter().filter(|r| r.action == "dispatched").count(),
            "would_dispatch": reports.iter().filter(|r| r.action == "would_dispatch").count(),
            "skipped": reports.iter().filter(|r| r.action == "skipped").count(),
            "failed": failed,
            "reports": reports.iter().map(SweepReport::to_json).collect::<Vec<_>>(),
        });
        let text = if reports.is_empty() {
            "no workspaces registered".to_string()
        } else {
            reports
                .iter()
                .map(SweepReport::to_line)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let payload = Payload::detail(doc, text);
        if failed > 0 {
            return Ok(payload.with_exit_code(1).into());
        }
        Ok(payload.into())
    }
}

fn sweep_workspace(
    global_root: &Path,
    ws: &Workspace,
    checkout: &WorkspaceCheckout,
    mode_override: Option<orbit_core::ShipMode>,
    dry_run: bool,
) -> SweepReport {
    if ws.status != WorkspaceStatus::Active || !checkout.orbit_dir.exists() {
        return SweepReport::skipped(ws, "workspace_inactive", 0);
    }
    // An explicit `--mode` override wins for every workspace; otherwise resolve
    // per-workspace from its registry entry (explicit `ship_mode`, else the
    // `pr` default). Workspaces that deliberately ship in-place must carry an
    // explicit `ship_mode = "local"`.
    let mode = mode_override.unwrap_or_else(|| orbit_core::resolved_ship_mode(ws));
    sweep_active_workspace(global_root, ws, checkout, mode, dry_run).unwrap_or_else(|error| {
        SweepReport {
            workspace_id: ws.id.clone(),
            workspace_name: ws.name.clone(),
            action: "error",
            reason: Some(error.to_string()),
            ready_backlog: 0,
            mode: None,
            run_id: None,
            run_state: None,
        }
    })
}

fn sweep_active_workspace(
    global_root: &Path,
    ws: &Workspace,
    checkout: &WorkspaceCheckout,
    mode: orbit_core::ShipMode,
    dry_run: bool,
) -> Result<SweepReport, OrbitError> {
    let runtime = RegisteredRuntimeFactory::open_registered_checkout(global_root, ws, checkout)?;
    if !runtime.workflow_auto_ship() {
        return Ok(SweepReport::skipped(ws, "auto_ship_disabled", 0));
    }

    let tasks = runtime.list_tasks()?;
    let status_by_id = runtime.task_status_index()?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    let ready_backlog = tasks
        .iter()
        .filter(|task| {
            task.status == TaskStatus::Backlog
                && task_dependencies_ready_with_index(task, &status_by_id, &reference_index)
        })
        .count();
    if ready_backlog == 0 {
        return Ok(SweepReport::skipped(ws, "no_ready_backlog", 0));
    }

    let in_flight = runtime
        .job_history(TASK_AUTO_PIPELINE_JOB)?
        .iter()
        .any(|run| {
            matches!(
                run.state,
                JobRunState::Pending | JobRunState::Running | JobRunState::Retrying
            )
        });
    if in_flight {
        return Ok(SweepReport::skipped(ws, "ship_in_flight", ready_backlog));
    }

    if dry_run {
        return Ok(SweepReport {
            workspace_id: ws.id.clone(),
            workspace_name: ws.name.clone(),
            action: "would_dispatch",
            reason: None,
            ready_backlog,
            mode: Some(mode.as_input_value()),
            run_id: None,
            run_state: None,
        });
    }

    // [ORB-10709] The sweep has no token of its own and does not force: an
    // operator holding the workspace claim is driving dispatch by hand, so the
    // unattended sweep stands down for that workspace and reports why, rather
    // than failing the whole run.
    let invoke = match runtime.submit_ship_run(
        mode,
        None,
        &[],
        // The sweep is an unattended routine: completion is only ever granted by
        // an operator on a single invocation, never implied by a schedule.
        orbit_core::CompletionPolicy::Review,
        &[],
        Some("ship-sweep"),
        None,
    ) {
        Ok(invoke) => invoke,
        Err(OrbitError::WorkspaceClaimHeld(claim)) => {
            return Ok(SweepReport::skipped(
                ws,
                &format!("workspace_claimed_by:{}", claim.holder),
                ready_backlog,
            ));
        }
        Err(error) => return Err(error),
    };
    Ok(SweepReport {
        workspace_id: ws.id.clone(),
        workspace_name: ws.name.clone(),
        action: "dispatched",
        reason: None,
        ready_backlog,
        mode: Some(mode.as_input_value()),
        run_id: Some(invoke.run_id),
        run_state: Some(if invoke.queued { "queued" } else { "submitted" }),
    })
}
