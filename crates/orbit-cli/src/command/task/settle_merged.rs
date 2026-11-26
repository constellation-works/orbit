use clap::Args;
use orbit_core::Core;
use orbit_core::app::task::{self, Command, Outcome, SettleMerged};
use orbit_core::application::task::{ExternalMergeSettlement, ExternalMergeSettlementProfile};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload};

/// `orbit task settle-merged` — settle tasks to done whose delivery pull
/// request was merged outside Orbit.
#[derive(Args)]
#[command(
    after_help = "A blocked, in-progress or review task settles when its Orbit delivery pull request is merged into its target branch and no run or claim of its own is live. The task records a landing note naming the pull request and merge commit. No work starts, so another task's claim on the same files does not refuse it.\nA handed-off claim whose completion authority an operator revoked settles with its task when exactly the handoff's pull request merged into the handoff's landing branch.\nAfter owner recovery, use --recovered-claim, --pull-request and --published-head together for a refused follower handoff whose pull request was rescued by hand. The original admission and delivery identity must match. History records the operator-reported published head, the merged head and the merge commit.\nEach `orbit run auto` pass does the ordinary settlement from the recently merged pull requests.\n\nExamples:\n  orbit task settle-merged             # settle every task whose delivery merged recently\n  orbit task settle-merged <task-id>   # settle one task, reading each of its pull requests\n  orbit task settle-merged <task-id> --recovered-claim <claim-id> --pull-request <number> --published-head <commit>"
)]
pub struct TaskSettleMergedArgs {
    /// Settle only this task, reading each pull request Orbit recorded for it.
    /// Without it, every candidate task is matched against the recently merged
    /// pull requests.
    #[arg(value_name = "TASK_ID")]
    pub id: Option<String>,
    /// Owner-recovered follower claim whose delivery was rescued by hand.
    #[arg(long, requires_all = ["id", "pull_request", "published_head"], value_name = "CLAIM_ID")]
    pub recovered_claim: Option<String>,
    /// Merged delivery pull request, even if its refused handoff recorded no PR.
    #[arg(long, requires = "recovered_claim", value_name = "NUMBER")]
    pub pull_request: Option<u64>,
    /// Full original published head commit, attested by the operator.
    #[arg(long, requires = "recovered_claim", value_name = "COMMIT")]
    pub published_head: Option<String>,
}

impl ExecuteCore for TaskSettleMergedArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let settle = match (
            self.id,
            self.recovered_claim,
            self.pull_request,
            self.published_head,
        ) {
            (Some(id), Some(claim), Some(pull_request), Some(published_head)) => {
                SettleMerged::RecoveredClaim {
                    id,
                    claim,
                    pull_request,
                    published_head,
                }
            }
            (_, Some(_), _, _) | (_, _, Some(_), _) | (_, _, _, Some(_)) => {
                return Err(orbit_core::OrbitError::InvalidInput(
                    "rescued settlement requires a task, --recovered-claim, --pull-request and --published-head".into(),
                ));
            }
            (Some(id), None, None, None) => SettleMerged::Task { id },
            (None, None, None, None) => SettleMerged::All,
        };
        let settled =
            task::exec(core, Command::SettleMerged(settle)).and_then(Outcome::into_settled)?;
        let rows: Vec<Value> = settled
            .iter()
            .map(|settlement| settlement.settlement_json(ExternalMergeSettlementProfile::Cli))
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
