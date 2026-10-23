//! `orbit run extend` — lengthen a live drain's window [ORB-15383].

use chrono::SecondsFormat;
use clap::Args;
use orbit_core::application::job::DrainWindowExtensionOutcome;
use orbit_core::{DrainWindowExtensionRequest, OrbitRuntime};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_duration_seconds;

#[derive(Args)]
#[command(
    about = "Lengthen a running drain's window, and optionally retune it",
    after_help = "Moves the deadline of a live `orbit run auto` window, or of a replica's \
`orbit run auto --pull` drain, to the later of its current deadline and now plus `--for`. The \
run ID, its completion authorization, and every task it has in flight are preserved, so a drain \
keeps admitting without being restarted. A second `orbit run auto` would instead queue behind \
the live drain until all of its work had settled.\n\nA window that already ends later is left \
as it is: an extension never shortens a drain. A pull drain can be extended after its window \
closed, while it still waits for its in-flight tasks to settle; it then resumes requesting \
work. An owner drain stops admitting once its window closes and is refused then.\n\nRefused: a \
drain whose admissions were stopped with `orbit run auto --stop` or that is being cancelled, \
since an extension never reopens one, and a drain started without `--for`, which makes a single \
admission pass and has no window to extend. A pull drain is refused too once a pass has \
closed its window with nothing left to settle: that drain is ending.\n\n`--concurrency` changes the ceiling as `orbit run concurrency --set` does.\n\
Owner/local drains count every in-flight child against N.\n\
A pull drain applies bounded before-landing review credit. Before each claim,\n\
it counts\n\
`occupied - min(reviewing, N)` against N.\n\
Reviewing begins after the owner's persisted before-landing admission has\n\
`applies=true`. Validation and publishing before that boundary count as\n\
implementing; later review revalidation and handoff remain reviewing.\n\
The credit is capped at N, but reviewing leaves can exceed N as\n\
replacements reach review.\n\
With an unchanged N and no pre-existing excess, occupied pull leaves stay\n\
at most 2N and implementing leaves at most N.\n\
Lowering N preserves running leaves, so existing excess can remain\n\
temporarily.\n\
See `orbit run show` for stage counts. Review rebuilds still compete for\n\
repository build-budget slots.\n\nExamples:\n  \
orbit run extend jrun-20260905-0546 --for 4h\n  orbit run extend jrun-20260905-0546 --for 2h \
--concurrency 5 --reason 'overnight backlog'"
)]
pub struct RunExtendArgs {
    /// Job run ID of the drain to extend
    pub run_id: String,

    /// How long from now the drain should keep admitting, e.g. `2h`
    #[arg(long = "for", value_name = "DURATION")]
    pub for_duration: String,

    /// Also set the admission ceiling; pull drains apply bounded review credit
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
            "concurrency": change.worker_limit.as_ref().map(|limit| json!({
                "outcome": limit.outcome,
                "previous_concurrency": limit.previous_max_active_leaf_runs,
                "concurrency": limit.max_active_leaf_runs,
                "revision": limit.revision,
            })),
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
