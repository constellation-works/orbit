//! `orbit run concurrency` — retune a live drain's worker ceiling [ORB-11253].

use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::DrainWorkerLimitRequest;
use orbit_core::app::run::{Command, exec};
use orbit_core::app::run::{DrainWorkerLimitChangeProfile, DrainWorkerLimitOutcome};

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
#[command(
    about = "Retune a running drain's admission ceiling",
    after_help = "Adjusts the worker ceiling of a live drain in place. The run ID, its deadline, \
its completion authorization and every child already dispatched are preserved. Raising the \
ceiling takes effect on the next admission pass. Lowering it never cancels a task already in \
flight. `--if-revision` refuses the change when the ceiling has moved.\n\n\
Examples:\n  \
orbit run concurrency jrun-20260905-0546 --set 7\n  \
orbit run concurrency jrun-20260905-0546 --set 3 --reason 'provider rate limited'\n  \
orbit run concurrency jrun-20260905-0546 --set 7 --if-revision 2 --json",
    after_long_help = "Adjusts the worker ceiling of a live `orbit run auto` window, or of a \
replica's `orbit run auto --pull` drain, in place. The run ID, its deadline, its completion \
authorization and every child already dispatched are preserved. Raising the ceiling takes \
effect on the next admission pass. Lowering it stops new admissions until enough children \
finish, and it never cancels a task already in flight. `--if-revision` refuses the change \
when the ceiling has moved: pass the revision `orbit run show` reported.\n\n\
Owner and local drains count every in-flight child against the ceiling. A pull drain applies \
a capped before-landing review credit and can keep more leaves than the ceiling while they \
are in review.\n\n\
How that credit is counted: installed orbit-orchestrate skill \
`references/orchestration-dispatch.md`.\n\n\
Examples:\n  \
orbit run concurrency jrun-20260905-0546 --set 7\n  \
orbit run concurrency jrun-20260905-0546 --set 3 --reason 'provider rate limited'\n  \
orbit run concurrency jrun-20260905-0546 --set 7 --if-revision 2 --json"
)]
pub struct RunConcurrencyArgs {
    /// Job run ID of the drain to retune
    pub run_id: String,

    /// New admission ceiling; pull drains apply bounded before-landing review credit
    #[arg(long = "set", value_name = "N")]
    pub set: u32,

    /// Note recorded with the change
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,

    /// Apply only while the ceiling is still at this revision
    #[arg(long = "if-revision", value_name = "N")]
    pub if_revision: Option<u32>,

    /// Token for this workspace's exclusive claim, when another operator holds
    /// one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`.
    #[arg(long)]
    pub claim_token: Option<String>,
}

impl ExecuteCore for RunConcurrencyArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let change = exec(
            core,
            Command::concurrency(DrainWorkerLimitRequest {
                run_id: &self.run_id,
                max_active_leaf_runs: self.set,
                expected_revision: self.if_revision,
                reason: self.reason.as_deref(),
                actor: "cli",
                source: "run_concurrency",
                claim_token: self.claim_token.as_deref(),
            }),
        )?
        .value;
        let doc = change.to_json(DrainWorkerLimitChangeProfile::Standalone);
        let text = if change.outcome == DrainWorkerLimitOutcome::Unchanged {
            format!(
                "job run {} already admits {} tasks at a time (revision {})",
                change.run_id, change.max_active_leaf_runs, change.revision
            )
        } else {
            format!(
                "job run {} now admits {} tasks at a time, was {} (revision {})\nRunning children are untouched; the new ceiling applies to the next admission pass.",
                change.run_id,
                change.max_active_leaf_runs,
                change.previous_max_active_leaf_runs,
                change.revision
            )
        };
        Ok(Payload::detail(doc, text).into())
    }
}
