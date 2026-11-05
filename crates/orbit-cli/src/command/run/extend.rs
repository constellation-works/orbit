//! `orbit run extend` — lengthen a live drain's window [ORB-15383].

use chrono::SecondsFormat;
use clap::Args;
use orbit_core::application::job::{DrainWindowExtensionOutcome, DrainWorkerLimitChangeProfile};
use orbit_core::{DrainWindowExtensionRequest, OrbitRuntime};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_duration_seconds;

#[derive(Args)]
#[command(
    about = "Lengthen a running drain's window, and optionally retune it",
    after_help = "Moves a live drain's deadline to the later of its current one and now plus \
`--for`, keeping its run ID, completion authorization and in-flight tasks. Extend a drain \
instead of starting another `orbit run auto`, which would queue behind it. An extension never \
shortens or reopens a drain.\n\
Refusals and how `--concurrency` counts pull-drain review: installed orbit-orchestrate skill \
`references/orchestration.md`.\n\nExamples:\n  \
orbit run extend jrun-20260905-0546 --for 4h\n  orbit run extend jrun-20260905-0546 --for 2h \
--concurrency 5 --reason 'overnight backlog'"
)]
pub struct RunExtendArgs {
    /// Job run ID of the drain to extend
    pub run_id: String,

    /// How long from now the drain should keep admitting, e.g. `2h`
    #[arg(long = "for", value_name = "DURATION")]
    pub for_duration: String,

    /// Also set the admission ceiling, as `orbit run concurrency --set` does
    #[arg(long, value_name = "N")]
    pub concurrency: Option<u32>,

    /// Note recorded with the change
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,

    /// Token for this workspace's exclusive claim, when another operator holds
    /// one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`.
    #[arg(long)]
    pub claim_token: Option<String>,
}

impl Execute for RunExtendArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let for_seconds = parse_duration_seconds(&self.for_duration)?;
        let change = runtime.extend_drain_window(DrainWindowExtensionRequest {
            run_id: &self.run_id,
            for_seconds,
            max_active_leaf_runs: self.concurrency,
            reason: self.reason.as_deref(),
            actor: "cli",
            source: "run_extend",
            claim_token: self.claim_token.as_deref(),
        })?;
        let previous = change
            .previous_deadline
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let deadline = change.deadline.to_rfc3339_opts(SecondsFormat::Secs, true);
        let doc = json!({
            "run_id": change.run_id,
            "job_id": change.job_id,
            "outcome": change.outcome,
            "previous_deadline": previous,
            "deadline": deadline,
            "revision": change.revision,
            "concurrency": change.worker_limit.as_ref().map(|limit| {
                limit.to_json(DrainWorkerLimitChangeProfile::Nested)
            }),
        });
        let mut text = if change.outcome == DrainWindowExtensionOutcome::Unchanged {
            format!(
                "job run {} already admits until {deadline}; a shorter window does not shorten it",
                change.run_id
            )
        } else {
            format!(
                "job run {} now admits until {deadline}, was {previous} (revision {})",
                change.run_id, change.revision
            )
        };
        if let Some(limit) = &change.worker_limit {
            text.push_str(&format!(
                "\nIt admits {} tasks at a time, was {} (revision {}).",
                limit.max_active_leaf_runs, limit.previous_max_active_leaf_runs, limit.revision
            ));
        }
        Ok(Payload::detail(doc, text).into())
    }
}
