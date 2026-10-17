use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::ExternalMergeSettlement;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

/// `orbit task settle-merged` — settle tasks to done whose delivery pull
/// request was merged outside Orbit.
#[derive(Args)]
#[command(
    after_help = "A blocked, in-progress or review task settles when its Orbit delivery pull request is merged into its target branch and no run or claim of its own is live. The task records a landing note naming the pull request and merge commit. No work starts, so another task's claim on the same files does not refuse it.\nEach `orbit run auto` pass does the same from the recently merged pull requests.\n\nExamples:\n  orbit task settle-merged             # settle every task whose delivery merged recently\n  orbit task settle-merged <task-id>   # settle one task, reading each of its pull requests"
)]
pub struct TaskSettleMergedArgs {
    /// Settle only this task, reading each pull request Orbit recorded for it.
    /// Without it, every candidate task is matched against the recently merged
    /// pull requests.
    #[arg(value_name = "TASK_ID")]
    pub id: Option<String>,
}

impl Execute for TaskSettleMergedArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let settled = match self.id.as_deref() {
            Some(id) => vec![runtime.settle_externally_merged_task(id)?],
            None => runtime.settle_externally_merged_tasks()?,
        };
        let rows: Vec<Value> = settled
            .iter()
            .map(|settlement| {
                json!({
                    "task_id": settlement.task_id,
                    "previous_status": settlement.previous_status,
                    "pr_number": settlement.pr_number,
                    "base_branch": settlement.base_branch,
                    "merge_commit": settlement.merge_commit,
                })
            })
            .collect();
        let doc = json!({ "settled": rows });
        let mut lines: Vec<String> = settled.iter().map(settled_line).collect();
        if lines.is_empty() {
            lines.push("no task has a delivery pull request merged outside Orbit".to_string());
        }
        Ok(Payload::detail(doc, lines.join("\n")).into())
    }
}

fn settled_line(settlement: &ExternalMergeSettlement) -> String {
    format!(
        "{}  {} -> done: pull request #{} merged into {} as {}",
        settlement.task_id,
        settlement.previous_status,
        settlement.pr_number,
        settlement.base_branch,
        settlement.merge_commit
    )
}
