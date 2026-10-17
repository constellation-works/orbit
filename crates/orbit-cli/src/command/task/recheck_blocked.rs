use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TransientInfraBlockedTask;
use orbit_core::runtime::InfraBlockedTask;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

/// `orbit task recheck-blocked` — re-check tasks blocked by a missing provider
/// launcher or by transient infrastructure, and requeue the cleared ones.
#[derive(Args)]
#[command(
    after_help = "Only blocks caused by a missing provider launcher or by transient infrastructure (a busy lock, a locked task store) are re-checked; a task blocked by its own failure stays blocked.\nThe launcher is resolved with dispatch's lookup from this shell's PATH and HOME. A transient block is cleared when the task store takes a write transaction and the task's lock is free; its next run resumes the candidate its failure handoff published. Each published candidate is requeued this way at most twice; after that use `orbit task requeue`.\n\nExamples:\n  orbit task recheck-blocked            # list infra-blocked tasks and whether each still reproduces\n  orbit task recheck-blocked --confirm  # return the cleared ones to backlog, with a history note"
)]
pub struct TaskRecheckBlockedArgs {
    /// Return the cleared tasks to backlog. Without it, only report.
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for TaskRecheckBlockedArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let found = runtime.infra_blocked_tasks()?;
        let transient = runtime.transient_infra_blocked_tasks()?;
        let (requeued, transient_requeued) = if self.confirm {
            (
                runtime.requeue_cleared_infra_blocked_tasks()?,
                runtime.requeue_cleared_transient_infra_blocks()?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let is_transient_requeued = |blocked: &TransientInfraBlockedTask| {
            transient_requeued
                .iter()
                .any(|task| task.task_id == blocked.task_id)
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
        let transient_rows: Vec<Value> = transient
            .iter()
            .map(|blocked| {
                json!({
                    "task_id": blocked.task_id,
                    "title": blocked.title,
                    "blocked_at": blocked.blocked_at.to_rfc3339(),
                    "run_id": blocked.run_id,
                    "failed_step_id": blocked.failed_step_id,
                    "class": blocked.class.as_str(),
                    "pull_request": blocked.pull_request,
                    "cleared": blocked.cleared,
                    "requeues": blocked.requeues,
                    "exhausted": blocked.exhausted,
                    "requeued": is_transient_requeued(blocked),
                })
            })
            .collect();
        let doc = json!({
            "confirm": self.confirm,
            "infra_blocked": rows,
            "transient_infra_blocked": transient_rows,
            "requeued": requeued.len() + transient_requeued.len(),
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
        lines.extend(transient.iter().map(|blocked| {
            let outcome = if is_transient_requeued(blocked) {
                "requeued onto its candidate".to_string()
            } else if blocked.exhausted {
                format!(
                    "left blocked; requeued {} time(s) already — `orbit task requeue` decides now",
                    blocked.requeues
                )
            } else if !blocked.cleared {
                "still reproduces".to_string()
            } else if self.confirm {
                "left blocked; the task changed, its lock is held, or its publication no longer authenticates".to_string()
            } else {
                "cleared".to_string()
            };
            format!(
                "{}  transient infrastructure ({}) at step `{}`: {outcome}",
                blocked.task_id,
                blocked.class.as_str(),
                blocked.failed_step_id
            )
        }));
        if !transient.is_empty() {
            lines.push(format!(
                "{} transient-infra-blocked task(s); {} returned to backlog",
                transient.len(),
                transient_requeued.len()
            ));
        }
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
