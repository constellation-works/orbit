//! `orbit sweep` — the stateless routine scheduler pass [ORB-10021].
//!
//! Invoked every minute by the OS clock (launchd / systemd; see
//! `orbit routine init --install-clock`). Like `orbit run ship-sweep`, it
//! resolves everything from the global registry, never bootstraps a
//! `.orbit/` in the caller's cwd, and exits non-zero on infrastructure
//! errors or when every discovered workspace fails to load. An unconfigured
//! host logs one line and exits 0, because the OS clock will invoke it
//! forever. Partial workspace load errors stay on stderr and still exit 0.

use std::path::Path;

use crate::command::{Block, CommandOut, Payload};
use clap::Args;
use orbit_cmd::registry_routines::{run_sweep, run_sweep_at};
use orbit_core::{
    OrbitError, OrbitRuntime,
    application::routines::{RoutineSweepReport, SweepOptions, SweepOutcome},
};
use serde_json::json;

#[derive(Args)]
#[command(
    name = "sweep",
    about = "Fire due routines on this host (the scheduler pass the OS clock invokes)",
    after_help = "Loads routine definitions from every registered, active owner checkout\n\
                  on this host and dispatches due targets as normal runs. Intended for\n\
                  the OS clock (launchd / systemd timer), e.g.:\n  orbit sweep --json\n\n\
                  By default only noteworthy rows (fires, retries, baselines, errors)\n\
                  print — the per-minute clock must not grow its log with `not_due`\n\
                  churn. Use --verbose for every routine's row.\n\n\
                  Pass the global `--workspace <selector>` to evaluate and fire only that\n\
                  registered workspace's routines.\n\n\
                  Inspect routines with `orbit routine list`; dispatched fires appear in\n\
                  `orbit run history`."
)]
pub struct SweepCommand {
    /// Report what would fire without recording or dispatching anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Print a row for every routine, including skipped/not-due ones.
    #[arg(long)]
    pub verbose: bool,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Actions worth a line on the once-a-minute clock path. The high-churn
/// `skipped` / `not_due` / `would_*` rows are suppressed unless `--verbose`
/// (or a dry-run, which is an interactive diagnostic) asks for everything —
/// otherwise a healthy host writes one line per routine per minute forever.
pub(crate) fn report_is_noteworthy(action: &str) -> bool {
    matches!(action, "fired" | "retry_fired" | "baselined" | "error")
}

/// Render one report row (used for both quiet and verbose output).
pub(crate) fn format_report_line(report: &RoutineSweepReport) -> String {
    let mut line = format!("{} ({}): {}", report.routine, report.source, report.action);
    if let Some(reason) = &report.reason {
        line.push_str(&format!(" — {reason}"));
    }
    if let Some(slot) = &report.slot {
        line.push_str(&format!(" — slot {slot}"));
    }
    if let Some(run_id) = &report.run_id {
        line.push_str(&format!(" — run {run_id}"));
    }
    line
}

impl SweepCommand {
    /// Runs without a pre-initialized runtime: the sweep resolves every
    /// workspace from the global registry (per-workspace runtimes are built
    /// inside orbit-core). The global `--workspace` selector, when present,
    /// narrows that set to one registered workspace.
    pub fn execute_without_runtime(
        self,
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        let options = SweepOptions {
            dry_run: self.dry_run,
            ..SweepOptions::default()
        };
        let workspace_selector = workspace_selector
            .map(str::trim)
            .filter(|selector| !selector.is_empty());
        let outcome = run_sweep_for_selected_root(root_override, workspace_selector, options)?;

        let doc = outcome_json(&outcome, self.dry_run);

        // Load errors are diagnostics, not records: they stay on stderr in
        // every mode so a `--format json` consumer still sees them. A pass
        // that opened zero workspaces collapses those rows into one
        // `sweep.no_workspace_loaded` line and exits non-zero [ORB-12244].
        if let Some(row) = &outcome.no_workspace_loaded {
            eprintln!("{row}");
        } else {
            for error in &outcome.load_errors {
                let path = error
                    .path
                    .as_ref()
                    .map(|path| format!(" ({})", path.display()))
                    .unwrap_or_default();
                eprintln!(
                    "load error [{}]{}: {}",
                    error.source_workspace, path, error.message
                );
            }
        }

        let lines = self.human_lines(&outcome);
        let exit_code = i32::from(outcome.no_workspace_loaded.is_some());
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))])
            .with_exit_code(exit_code)
            .into())
    }

    /// The `table`/plain lines for one pass.
    fn human_lines(&self, outcome: &SweepOutcome) -> Vec<String> {
        if outcome.lock_busy {
            return vec!["sweep: another pass holds the lock on this host; exiting".to_string()];
        }
        if outcome.reports.is_empty() && outcome.load_errors.is_empty() {
            return vec![format!(
                "sweep[{}]: no routines configured",
                outcome.host_id
            )];
        }

        // Quiet by default; a dry-run is interactive so it shows everything.
        let show_all = self.verbose || self.dry_run;
        let mut lines = Vec::new();
        for report in &outcome.reports {
            if show_all || report_is_noteworthy(report.action) {
                lines.push(format_report_line(report));
            }
        }
        // A one-line heartbeat when a healthy pass had nothing to report, so the
        // log still shows the sweep ran (bounded by the log rotation in
        // `run_sweep`) without a row per routine.
        if lines.is_empty() && outcome.load_errors.is_empty() {
            lines.push(format!(
                "sweep[{}]: {} routine(s), nothing due",
                outcome.host_id,
                outcome.reports.len()
            ));
        }
        lines
    }
}

fn run_sweep_for_selected_root(
    root_override: Option<&Path>,
    workspace_selector: Option<&str>,
    options: SweepOptions,
) -> Result<SweepOutcome, OrbitError> {
    let has_env_override = std::env::var("ORBIT_ROOT").is_ok_and(|root| !root.trim().is_empty());
    if root_override.is_none() && !has_env_override {
        return run_sweep(options, workspace_selector);
    }

    let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
    let roots = OrbitRuntime::resolve_roots_for_cwd(&cwd, root_override)?;
    run_sweep_at(&roots.global_root, options, workspace_selector)
}

pub(crate) fn outcome_json(outcome: &SweepOutcome, dry_run: bool) -> serde_json::Value {
    json!({
        "host_id": outcome.host_id,
        "machine_id": outcome.machine_id,
        "dry_run": dry_run,
        "lock_busy": outcome.lock_busy,
        "fired": outcome
            .reports
            .iter()
            .filter(|r| r.action == "fired" || r.action == "retry_fired")
            .count(),
        "reports": outcome.reports.iter().map(|r| json!({
            "routine": r.routine,
            "source": r.source,
            "origin": r.origin,
            "action": r.action,
            "reason": r.reason,
            "slot": r.slot,
            "run_id": r.run_id,
        })).collect::<Vec<_>>(),
        "load_errors": outcome.load_errors.iter().map(|e| json!({
            "source_workspace": e.source_workspace,
            "path": e.path.as_ref().map(|p| p.display().to_string()),
            "message": e.message,
        })).collect::<Vec<_>>(),
        "no_workspace_loaded": outcome.no_workspace_loaded,
    })
}
