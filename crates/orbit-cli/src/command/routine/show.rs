use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_cmd::registry_routines::routine_statuses;
use orbit_core::OrbitError;
use orbit_core::application::routines::{
    OwnerOnlyRoutine, RetiredRoutine, RoutineStatusReport, recent_fires,
};
use orbit_types::workflow::automation::members::BatchMember;
use serde_json::json;

const RECENT_FIRE_LIMIT: usize = 10;

#[derive(Args)]
pub struct RoutineShowArgs {
    /// Routine name.
    pub name: String,
}

impl RoutineShowArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        let report = routine_statuses(global_root)?;
        let Some(status) = report
            .statuses
            .iter()
            .find(|status| status.routine.definition.name == self.name)
        else {
            // `routine list` hides a routine whose plugin is off; it still
            // resolves here, reported as inactive with the reason.
            if let Some(routine) = report
                .inactive_plugin_routines()
                .find(|routine| routine.name == self.name)
            {
                return Ok(inactive_detail(&report, routine).into());
            }
            // A replica lists what its owner schedules; it resolves here with
            // the owner-authority reason.
            if let Some(owned) = report
                .owner_only
                .iter()
                .find(|owned| owned.routine.definition.name == self.name)
            {
                return Ok(owner_only_detail(&report, owned).into());
            }
            return Err(OrbitError::InvalidInput(format!(
                "no routine named '{}' (see `orbit routine list`)",
                self.name
            )));
        };
        let fires = recent_fires(global_root, &self.name, RECENT_FIRE_LIMIT)?;
        let definition = &status.routine.definition;

        let doc = json!({
            "machine_name": report.machine_name,
            "machine_id": report.machine_id,
            "name": definition.name,
            "description": definition.description,
            "source": status.routine.source_workspace,
            "origin": status.routine.origin.as_str(),
            "path": status.routine.path.display().to_string(),
            "enabled": definition.enabled,
            "paused_at": status.paused_at,
            "effective": status.effective(),
            "cron": definition.trigger.cron,
            "trigger": definition.trigger,
            "automation": status.automation,
            "missed_run": definition.trigger.missed_run,
            "target": definition.target.as_ref_string(),
            "policy": definition.policy,
            "next_due": status.next_due,
            "recent_fires": fires.iter().map(|fire| json!({
                "slot": fire.slot,
                "attempt": fire.attempt,
                "state": fire.state.as_str(),
                "run_id": fire.run_id,
                "detail": fire.detail,
                "updated_at": fire.updated_at,
            })).collect::<Vec<_>>(),
        });

        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "Name: {}", definition.name);
        if !definition.description.is_empty() {
            let _ = writeln!(out, "Description: {}", definition.description);
        }
        let _ = writeln!(
            out,
            "Source: {} ({}, {} origin)",
            status.routine.source_workspace,
            status.routine.path.display(),
            status.routine.origin.as_str()
        );
        let _ = writeln!(out, "Target: {}", definition.target.as_ref_string());
        if let Some(diagnostic) = &status.automation {
            let _ = writeln!(
                out,
                "Automation: {}",
                serde_json::to_string_pretty(diagnostic)
                    .map_err(|e| OrbitError::InvalidInput(e.to_string()))?
            );
        }

        if let Some(trigger) = &definition.trigger.state {
            let _ = writeln!(
                out,
                "State trigger: {}",
                serde_json::to_string(trigger)
                    .map_err(|e| OrbitError::InvalidInput(e.to_string()))?
            );
            let batch = status
                .automation
                .as_ref()
                .and_then(|diagnostic| diagnostic.get("batch"))
                .map(|batch| serde_json::from_value::<Vec<BatchMember>>(batch.clone()))
                .transpose()
                .map_err(|e| OrbitError::InvalidInput(e.to_string()))?
                .unwrap_or_default();
            if !batch.is_empty() {
                let _ = writeln!(
                    out,
                    "Batch ({} of up to {}): {}",
                    batch.len(),
                    trigger.effective_batch_size(),
                    crate::command::clock::tick::format_batch(&batch)
                );
            }
        } else {
            let _ = writeln!(
                out,
                "Trigger: cron \"{}\" (missed_run: {})",
                definition.trigger.cron,
                match definition.trigger.missed_run {
                    orbit_core::MissedRunPolicy::CatchUpOnce => "catch_up_once",
                    orbit_core::MissedRunPolicy::Skip => "skip",
                }
            );
        }
        let _ = writeln!(
            out,
            "Policy: timeout {}m, retries max {} (backoff {}m), overlap {}",
            definition.policy.timeout_minutes,
            definition.policy.retries.max,
            definition.policy.retries.backoff_minutes,
            match definition.policy.overlap {
                orbit_core::OverlapPolicy::Forbid => "forbid",
                orbit_core::OverlapPolicy::Allow => "allow",
            }
        );
        let _ = writeln!(
            out,
            "Enabled: {} | Paused: {}",
            definition.enabled,
            status
                .paused_at
                .as_deref()
                .map(|at| format!("yes (since {at})"))
                .unwrap_or_else(|| "no".to_string())
        );
        let _ = writeln!(
            out,
            "Effective on this host: {}",
            if status.effective() { "yes" } else { "no" }
        );
        let _ = writeln!(
            out,
            "Next due: {}",
            status.next_due.as_deref().unwrap_or("unknown")
        );
        if fires.is_empty() {
            let _ = writeln!(out, "Recent fires: none");
        } else {
            let _ = writeln!(out, "Recent fires:");
            for fire in &fires {
                let run = fire
                    .run_id
                    .as_deref()
                    .map(|run_id| format!(" run {run_id}"))
                    .unwrap_or_default();
                let detail = fire
                    .detail
                    .as_deref()
                    .map(|detail| format!(" — {detail}"))
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "  [{}] slot {} attempt {}{}{}",
                    fire.state.as_str(),
                    fire.slot,
                    fire.attempt,
                    run,
                    detail
                );
            }
        }
        Ok(Payload::detail(doc, out).into())
    }
}

/// A routine seeded by a plugin that is off where it lives: no schedule state,
/// only where it is and why it never fires.
fn inactive_detail(report: &RoutineStatusReport, routine: &RetiredRoutine) -> Payload {
    let doc = json!({
        "machine_name": report.machine_name,
        "machine_id": report.machine_id,
        "name": routine.name,
        "source": routine.source_workspace,
        "origin": routine.origin.as_str(),
        "path": routine.path.display().to_string(),
        "target": format!("job:{}", routine.job),
        "effective": false,
        "plugin_inactive": true,
        "skipped_reason": routine.reason,
    });
    let out = format!(
        "Name: {}\nSource: {} ({}, {} origin)\nTarget: job:{}\nEffective on this host: no\n\
         Inactive: {}\n",
        routine.name,
        routine.source_workspace,
        routine.path.display(),
        routine.origin.as_str(),
        routine.job,
        routine.reason
    );
    Payload::detail(doc, out)
}

fn owner_only_detail(report: &RoutineStatusReport, owned: &OwnerOnlyRoutine) -> Payload {
    let routine = &owned.routine;
    let target = routine.definition.target.as_ref_string();
    let doc = json!({
        "machine_name": report.machine_name,
        "machine_id": report.machine_id,
        "name": routine.definition.name,
        "source": routine.source_workspace,
        "origin": routine.origin.as_str(),
        "path": routine.path.display().to_string(),
        "target": target,
        "enabled": routine.definition.enabled,
        "effective": false,
        "owner_only": true,
        "owner_machine": owned.owner_machine,
        "skipped_reason": owned.reason,
    });
    let out = format!(
        "Name: {}\nSource: {} ({}, {} origin)\nTarget: {}\nEffective on this host: no\n\
         Owner-only: {}\n",
        routine.definition.name,
        routine.source_workspace,
        routine.path.display(),
        routine.origin.as_str(),
        target,
        owned.reason
    );
    Payload::detail(doc, out)
}
