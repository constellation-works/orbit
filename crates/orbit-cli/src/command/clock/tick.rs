//! The stateless scheduler tick shared by `orbit clock tick` and `orbit sweep`.

use std::path::Path;

use clap::Args;
use orbit_cmd::registry_routines::{run_sweep, run_sweep_at};
use orbit_core::{
    OrbitError, OrbitRuntime,
    application::routines::{
        AutoTaskSweepReport, RoutineSweepReport, SweepOptions, SweepOutcome,
        clock_unit_drift_warning,
    },
};
use orbit_types::workflow::automation::members::BatchMember;
use serde_json::json;

use crate::command::{Block, CommandOut, Payload};

#[derive(Args)]
#[command(
    about = "Evaluate due routines and auto-tasks on this host",
    after_help = "Loads routines and auto-task definitions from every registered, active owner\n\
                  checkout on this host. Due routines dispatch normal runs; due auto-tasks\n\
                  mint tasks in-process. The OS clock invokes this pass, for example:\n  orbit clock tick --json\n\n\
                  By default only noteworthy rows print. Use --verbose for every row.\n\n\
                  Pass the global `--workspace <selector>` to evaluate only that workspace."
)]
pub struct ClockTickArgs {
    /// Report what would fire without recording, dispatching, or minting anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Print every routine and auto-task row, including skipped/not-due ones.
    #[arg(long)]
    pub verbose: bool,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

pub(crate) fn report_is_noteworthy(action: &str) -> bool {
    matches!(
        action,
        "fired" | "retry_fired" | "baselined" | "error" | "minted"
    )
}

pub(crate) fn format_routine_report_line(report: &RoutineSweepReport) -> String {
    let mut line = format!("{} ({}): {}", report.routine, report.source, report.action);
    append_report_details(&mut line, &report.reason, &report.slot);
    if let Some(run_id) = &report.run_id {
        line.push_str(&format!(" — run {run_id}"));
    }
    if !report.batch.is_empty() {
        line.push_str(&format!(" — batch [{}]", format_batch(&report.batch)));
    }
    line
}

/// `key (reason)` per batch member, in admission order.
pub(crate) fn format_batch(batch: &[BatchMember]) -> String {
    batch
        .iter()
        .map(|member| format!("{} ({})", member.key, member.reason))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn format_auto_task_report_line(report: &AutoTaskSweepReport) -> String {
    let mut line = format!(
        "{} ({}, auto-task): {}",
        report.name, report.source, report.action
    );
    append_report_details(&mut line, &report.reason, &report.slot);
    if let Some(task_id) = &report.task_id {
        line.push_str(&format!(" — task {task_id}"));
    }
    line
}

fn append_report_details(line: &mut String, reason: &Option<String>, slot: &Option<String>) {
    if let Some(reason) = reason {
        line.push_str(&format!(" — {reason}"));
    }
    if let Some(slot) = slot {
        line.push_str(&format!(" — slot {slot}"));
    }
}

impl ClockTickArgs {
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
        // The scheduled pass runs the program the unit names, so this only
        // fires for a hand-run pass from another install — which is exactly
        // the evidence that unattended ticks are using a different binary.
        if let Some(warning) = clock_unit_drift_warning() {
            eprintln!("{warning}");
        }

        let outcome = run_sweep_for_selected_root(root_override, workspace_selector, options)?;
        let document = outcome_json(&outcome, self.dry_run);

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
        Ok(
            Payload::blocks(document, vec![Block::text(lines.join("\n"))])
                .with_exit_code(exit_code)
                .into(),
        )
    }

    fn human_lines(&self, outcome: &SweepOutcome) -> Vec<String> {
        if outcome.lock_busy {
            return vec![
                "clock tick: another pass holds the lock on this host; exiting".to_string(),
            ];
        }
        if outcome.reports.is_empty()
            && outcome.auto_task_reports.is_empty()
            && outcome.load_errors.is_empty()
        {
            return vec![format!(
                "clock tick[{}]: no schedules configured",
                outcome.machine_name
            )];
        }

        let show_all = self.verbose || self.dry_run;
        let mut lines = Vec::new();
        for report in &outcome.reports {
            if show_all
                || report_is_noteworthy(report.action)
                || report
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with("workspace_drain_live:"))
            {
                lines.push(format_routine_report_line(report));
            }
        }
        for report in &outcome.auto_task_reports {
            if show_all || report_is_noteworthy(report.action) {
                lines.push(format_auto_task_report_line(report));
            }
        }
        if lines.is_empty() && outcome.load_errors.is_empty() {
            // Retired rows are definitions the tick skips, not schedules.
            let evaluated = outcome
                .reports
                .iter()
                .filter(|report| report.action != "retired")
                .count();
            lines.push(format!(
                "clock tick[{}]: {} routine(s), {} auto-task(s), nothing due",
                outcome.machine_name,
                evaluated,
                outcome.auto_task_reports.len()
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
    let mut reports = outcome
        .reports
        .iter()
        .map(|report| {
            json!({
                "kind": "routine",
                "name": report.routine,
                "routine": report.routine,
                "source": report.source,
                "origin": report.origin,
                "action": report.action,
                "reason": report.reason,
                "slot": report.slot,
                "run_id": report.run_id,
                "task_id": null,
                "batch": report.batch,
            })
        })
        .collect::<Vec<_>>();
    reports.extend(outcome.auto_task_reports.iter().map(|report| {
        json!({
            "kind": "auto_task",
            "name": report.name,
            "source": report.source,
            "origin": null,
            "action": report.action,
            "reason": report.reason,
            "slot": report.slot,
            "run_id": null,
            "task_id": report.task_id,
        })
    }));

    json!({
        "machine_name": outcome.machine_name,
        "machine_id": outcome.machine_id,
        "dry_run": dry_run,
        "lock_busy": outcome.lock_busy,
        "fired": outcome.reports.iter().filter(|report| {
            report.action == "fired" || report.action == "retry_fired"
        }).count(),
        "minted": outcome.auto_task_reports.iter().filter(|report| report.action == "minted").count(),
        "reports": reports,
        "load_errors": outcome.load_errors.iter().map(|error| json!({
            "source_workspace": error.source_workspace,
            "path": error.path.as_ref().map(|path| path.display().to_string()),
            "message": error.message,
        })).collect::<Vec<_>>(),
        "no_workspace_loaded": outcome.no_workspace_loaded,
    })
}
