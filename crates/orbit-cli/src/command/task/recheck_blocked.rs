use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::InfraBlockedTask;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

/// `orbit task recheck-blocked` — re-check tasks blocked by a missing provider
/// launcher and requeue the ones whose launcher now resolves.
#[derive(Args)]
#[command(
    after_help = "Only blocks caused by a missing provider launcher are re-checked; a task blocked by its own failure stays blocked.\nThe launcher is resolved with dispatch's lookup from this shell's PATH and HOME.\n\nExamples:\n  orbit task recheck-blocked            # list infra-blocked tasks and whether each still reproduces\n  orbit task recheck-blocked --confirm  # return the cleared ones to backlog, with a history note"
)]
pub struct TaskRecheckBlockedArgs {
    /// Return tasks whose launcher now resolves to backlog. Without it, only report.
    #[arg(long)]
    pub confirm: bool,
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskRecheckBlockedArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let found = runtime.infra_blocked_tasks()?;
        let requeued = if self.confirm {
            runtime.requeue_cleared_infra_blocked_tasks()?
        } else {
            Vec::new()
        };
        let is_requeued = |blocked: &InfraBlockedTask| {
            requeued.iter().any(|task| task.task_id == blocked.task_id)
        };

        let rows: Vec<Value> = found
            .iter()
            .map(|blocked| {
                json!({
                    "task_id": blocked.task_id,
                    "title": blocked.title,
                    "blocked_at": blocked.blocked_at.to_rfc3339(),
                    "run_id": blocked.run_id,
                    "program": blocked.program,
                    "provider": blocked.provider,
                    "launcher": blocked.launcher.as_ref().map(|path| path.display().to_string()),
                    "requeued": is_requeued(blocked),
                })
            })
            .collect();
        let doc = json!({
            "confirm": self.confirm,
            "infra_blocked": rows,
            "requeued": requeued.len(),
        });

        let mut lines: Vec<String> = found
            .iter()
            .map(|blocked| {
                let outcome = match (&blocked.launcher, is_requeued(blocked)) {
                    (Some(path), true) => format!("requeued; now resolves at {}", path.display()),
                    (Some(path), false) if self.confirm => format!(
                        "left blocked; resolves at {} but the block changed since the scan",
                        path.display()
                    ),
                    (Some(path), false) => format!("cleared; resolves at {}", path.display()),
                    (None, _) => "still missing".to_string(),
                };
                format!(
                    "{}  `{}` for provider `{}`: {outcome}",
                    blocked.task_id, blocked.program, blocked.provider
                )
            })
            .collect();
        let cleared = found
            .iter()
            .filter(|blocked| blocked.launcher.is_some())
            .count();
        lines.push(if found.is_empty() {
            "no task is blocked by a missing provider launcher".to_string()
        } else if self.confirm {
            format!(
                "{} infra-blocked task(s); {} returned to backlog",
                found.len(),
                requeued.len()
            )
        } else if cleared > 0 {
            format!(
                "{} infra-blocked task(s); {cleared} cleared — run with --confirm to return them to backlog",
                found.len()
            )
        } else {
            format!(
                "{} infra-blocked task(s); none cleared — install the launcher (see `orbit doctor providers`) and re-run",
                found.len()
            )
        });
        Ok(Payload::detail(doc, lines.join("\n")).into())
    }
}
