//! `orbit run auto` workspace logistics entrypoint.

use clap::Args;
use orbit_core::{
    CompletionPolicy, DrainAdmissionsStopRequest, OrbitRuntime, WorkspacePullRequest,
};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_duration_seconds;

use super::support::{WorkflowDispatchResult, workflow_dispatch_payload_with_notices};

pub(super) const AUTO_WORKFLOW: &str = "auto";

#[derive(Args)]
#[command(
    about = "Drain the workspace backlog for a window",
    override_usage = "orbit run auto [OPTIONS]",
    after_help = "Examples:\n  orbit run auto\n  orbit run auto --medium-complexity-crews grok,sol\n  orbit run auto --for 4h\n  orbit run auto --for 4h --concurrency 8\n  orbit run auto --for 4h --complete\n  orbit run auto --for 4h --approve-proposed\n  orbit run auto --stop\n  orbit run auto --pull hm_owner/ws_orbit --for 8h --concurrency 3\n  orbit run auto --pull hm_owner/ws_orbit --for 8h --allow-crew sol,luna\n\n\
                  The drain re-lists the whole backlog every pass and keeps `--concurrency`\n\
                  tasks in flight, starting a replacement as each one finishes rather than\n\
                  waiting for the batch.\n\n\
                  `--complete` is blanket authorization: it applies to every task the drain\n\
                  admits for the whole window, including work that reaches the backlog after\n\
                  the run starts. The drain is asynchronous, so this prints the durable run ID\n\
                  and returns without knowing the eventual outcome.\n\n\
                  `--complete` never approves `proposed` work; `--approve-proposed` does.\n\
                  With it, every pass first pilots the qualifying proposed tasks (tagged\n\
                  `no-diff-expected`, or with context files and an assessed complexity) and\n\
                  approves each one the pilot verifies: no duplicate, already-landed,\n\
                  conflict or warning finding. That includes tasks filed after the window\n\
                  opens, and the same pass can admit them. Each approval's history note names\n\
                  the drain run. Other tasks stay proposed; `orbit run show` and\n\
                  `orbit run readiness` count the approvals and give each hold's reason.\n\
                  A task tagged `no-auto-approve` is never approved automatically; it is held\n\
                  with that reason until a human approves it.\n\
                  Approval does not bypass `--allow-crew` or the complexity pools.\n\n\
                  Complexity pools select only for tasks without an explicit crew.\n\
                  The tiers are low, medium, hard and xhard; xhard is the reserved top tier.\n\
                  Each CLI pool replaces its matching workflow pool for this drain.\n\
                  Empty pools and unset complexity use the existing default crew chain.\n\
                  Selections are recorded at admission and retained on retries/resume.\n\
                  Pools do not restrict manual crew choices.\n\n\
                  `--allow-crew` restricts this one run to the named crews, for its window\n\
                  only. It edits no configuration and reassigns nothing: a backlog task whose\n\
                  crew is excluded is simply not started, and `orbit run readiness --allow-crew`\n\
                  names it. To actually move that work, reassign its crew yourself. Tasks a\n\
                  different invocation already has in flight keep running.\n\n\
                  With `--pull`, `--allow-crew` limits the crews this replica declares to the\n\
                  owner for the window, so the owner never hands it a task on another crew;\n\
                  that work stays in the owner's backlog. The owner's before-PR review crew is\n\
                  not restricted by it, but must still run here.\n\n\
                  `--pull <SELECTOR>` runs on a replica checkout instead. The owner named by\n\
                  the host-qualified selector orders the work and admits one claim at a\n\
                  time; each claim runs here as a leaf that ends at a pull request handed\n\
                  back to the owner, which keeps landing authority. The selector must name\n\
                  this replica's own owner and workspace, and the owner's probe must admit\n\
                  this executor, before anything is submitted. Without `--for` (or with\n\
                  `--for 0s`) it makes one admission pass, claiming up to `--concurrency`\n\
                  tasks, and requests no replacements. The drain keeps settling its\n\
                  claims with the owner after the window closes, until none is left. Each\n\
                  leaf also delivers its own handoff or failure when it ends, so a leaf\n\
                  still settles if its drain was stopped or cancelled.\n\n\
                  `--stop` ends new admissions for this workspace's active auto coordinator.\n\
                  You do not need a run ID. Already admitted workers keep running under the\n\
                  completion authority they were started with; this is not cancellation.\n\
                  To cancel those workers, `orbit run cancel <RUN_ID> --confirm` each child.\n\
                  On a replica, `--stop` also delivers every pull settlement still recorded\n\
                  for any owner, and ends unlaunched claims that no running pull drain will\n\
                  carry, so it is also how to flush settlements an earlier, cancelled drain\n\
                  left behind. Otherwise a second `--stop`, or `--stop` with no active\n\
                  coordinator, is a no-op.\n\n\
                  Inspect submitted runs with `orbit run history -j workspace_auto_pipeline` and\n\
                  `orbit run show <RUN_ID>`."
)]
pub struct AutoCommand {
    /// How long to keep draining, e.g. `30m`, `2h`. Without it, or with
    /// `0s`, the run makes one admission pass and admits nothing more. The
    /// window bounds only the start of new work: a task already being shipped
    /// when it expires still finishes.
    #[arg(long = "for", value_name = "DURATION")]
    pub for_duration: Option<String>,
    /// How many tasks may be in flight at once. The drain tops these slots up
    /// from the whole backlog as each one frees, so this is the parallelism,
    /// not a batch size. Defaults to 5.
    #[arg(long, value_name = "N")]
    pub concurrency: Option<u32>,
    /// Authorize this drain to finish delivery and move the tasks it ships to
    /// `done`, instead of leaving them in `review` for a separate approval.
    /// This is blanket authorization for every task the drain admits during its
    /// whole window, not just the backlog visible right now. Off by default.
    /// It does not approve `proposed` work; `--approve-proposed` does that.
    #[arg(long)]
    pub complete: bool,
    /// Approve qualifying `proposed` tasks into the backlog on every pass of
    /// this drain, including tasks filed while the window is open. A task
    /// qualifies with the `no-diff-expected` tag, or with context files and
    /// an assessed complexity. It must then pass task-pilot verification:
    /// no duplicate, already-landed, conflict or warning finding. Tasks that
    /// fail, and tasks tagged `no-auto-approve`, stay proposed, and the drain
    /// reports why. Off by default.
    #[arg(long)]
    pub approve_proposed: bool,
    /// Restrict this run to these configured crews, e.g. when a provider is
    /// unavailable or its budget is spent. Repeatable and comma-separated.
    /// Every name must be configured here; an unknown or empty one fails
    /// before anything is dispatched. Omitted, the drain runs every crew, as
    /// before. This is scoped to this run's window only — no workspace
    /// configuration is changed, no task is reassigned, and nothing another
    /// invocation is already running is cancelled. With `--pull`, only tasks
    /// on these crews are claimed from the owner.
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Require a systemd user scope for the coordinator and every leaf worker
    /// it starts. Overrides machine.worker_containment_strict for this drain.
    #[arg(long)]
    pub strict_worker_containment: bool,
    /// Random crew pool for unassigned low-complexity tasks. Entries are
    /// `crew` or `crew:weight` (relative, non-negative whole numbers), all
    /// bare or all weighted. Overrides the matching workflow pool; pass the
    /// flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub low_complexity_crews: Option<Vec<String>>,
    /// Random crew pool for unassigned medium-complexity tasks. Entries are
    /// `crew` or `crew:weight` (relative, non-negative whole numbers), all
    /// bare or all weighted. Overrides the matching workflow pool; pass the
    /// flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub medium_complexity_crews: Option<Vec<String>>,
    /// Random crew pool for unassigned hard-complexity tasks. Entries are
    /// `crew` or `crew:weight` (relative, non-negative whole numbers), all
    /// bare or all weighted. Overrides the matching workflow pool; pass the
    /// flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub hard_complexity_crews: Option<Vec<String>>,
    /// Random crew pool for unassigned xhard-complexity tasks, the reserved
    /// top tier. Entries are `crew` or `crew:weight` (relative, non-negative
    /// whole numbers), all bare or all weighted. Overrides the matching
    /// workflow pool; pass the flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub xhard_complexity_crews: Option<Vec<String>>,
    /// Pull from this owner instead of draining a local backlog. Takes the
    /// owner's host-qualified selector from federated discovery and runs only
    /// on that owner's replica checkout. Pulled work always stops at a handoff
    /// the owner lands; `--complete` and the complexity pools do not apply,
    /// and `--approve-proposed` is refused because only the owner approves
    /// work. `--allow-crew` limits which tasks are claimed.
    #[arg(
        long,
        value_name = "SELECTOR",
        conflicts_with_all = ["complete", "approve_proposed", "strict_worker_containment", "low_complexity_crews", "medium_complexity_crews", "hard_complexity_crews", "xhard_complexity_crews", "claim_token"]
    )]
    pub pull: Option<String>,
    /// Owner host for `--pull <workspace>`, by registered host name or
    /// `machine_id` (see `orbit host list`). Orbit reads that host's live
    /// workspace list and pulls from the selector it lists. Without it,
    /// `--pull` takes only a full host-qualified selector.
    #[arg(long, value_name = "HOST", requires = "pull")]
    pub host: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
    /// Token for this workspace's exclusive claim, when another operator holds
    /// one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`.
    #[arg(long)]
    pub claim_token: Option<String>,
    /// Stop new admissions for this workspace's active auto coordinator.
    /// Already admitted workers keep running. Conflicts with the flags that
    /// start a drain.
    #[arg(
        long,
        conflicts_with_all = ["for_duration", "concurrency", "complete", "approve_proposed", "allow_crew", "strict_worker_containment", "low_complexity_crews", "medium_complexity_crews", "hard_complexity_crews", "xhard_complexity_crews", "pull"]
    )]
    pub stop: bool,
}

impl Execute for AutoCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.stop {
            return execute_stop(runtime, self.claim_token.as_deref());
        }
        if let Some(selector) = self.pull.as_deref() {
            let for_seconds = self
                .for_duration
                .as_deref()
                .map(parse_duration_seconds)
                .transpose()?;
            let invoke = runtime.submit_workspace_pull_run(
                WorkspacePullRequest {
                    selector,
                    for_seconds,
                    max_active_leaf_runs: self.concurrency,
                    allowed_crews: &self.allow_crew,
                    actor: None,
                },
                orbit_types::workflow::JobRunTrigger::cli(),
            )?;
            return workflow_dispatch_payload_with_notices(
                AUTO_WORKFLOW,
                &[WorkflowDispatchResult {
                    workflow_alias: AUTO_WORKFLOW,
                    job_id: invoke.job_name,
                    run_id: invoke.run_id,
                    state: if invoke.queued {
                        "queued".to_string()
                    } else {
                        "submitted".to_string()
                    },
                    attempt: 1,
                    error_code: None,
                    error_message: None,
                }],
                submission_warnings(runtime),
                runtime.required_validation_note().into_iter().collect(),
            );
        }
        let complexity_crews = orbit_config::ComplexityCrewPools {
            low: self.low_complexity_crews,
            medium: self.medium_complexity_crews,
            hard: self.hard_complexity_crews,
            xhard: self.xhard_complexity_crews,
        };
        let for_seconds = self
            .for_duration
            .as_deref()
            .map(parse_duration_seconds)
            .transpose()?;
        let completion = if self.complete {
            CompletionPolicy::Done
        } else {
            CompletionPolicy::Review
        };
        let invoke = runtime.submit_workspace_auto_run_with_containment(
            for_seconds,
            self.concurrency,
            completion,
            &self.allow_crew,
            &complexity_crews,
            None,
            self.claim_token.as_deref(),
            orbit_types::workflow::JobRunTrigger::cli(),
            self.strict_worker_containment,
            self.approve_proposed,
        )?;
        let run = WorkflowDispatchResult {
            workflow_alias: AUTO_WORKFLOW,
            job_id: invoke.job_name,
            run_id: invoke.run_id,
            state: if invoke.queued {
                "queued".to_string()
            } else {
                "submitted".to_string()
            },
            attempt: 1,
            error_code: None,
            error_message: None,
        };
        workflow_dispatch_payload_with_notices(
            AUTO_WORKFLOW,
            &[run],
            owner_drain_warnings(runtime),
            runtime.required_validation_note().into_iter().collect(),
        )
    }
}

/// What a drain submission warns about while still starting: a host resource
/// throttle, and a validation environment that may not find the user's
/// toolchain [ORB-13987].
fn submission_warnings(runtime: &OrbitRuntime) -> Vec<String> {
    resource_throttle_warning(runtime)
        .into_iter()
        .chain(runtime.validation_env_preflight_warning())
        .collect()
}

/// A local drain's warnings, plus the backlog tasks this host's OS cannot
/// start: they stay waiting for a host of theirs, named here so the drain
/// does not read as idle over an empty backlog.
fn owner_drain_warnings(runtime: &OrbitRuntime) -> Vec<String> {
    let mut warnings = submission_warnings(runtime);
    warnings.extend(runtime.host_os_backlog_warning());
    warnings
}

/// [ORB-13901] The drain starts either way and holds its own waves while the
/// host is throttled; say so at start rather than leaving an idle drain to be
/// read as an empty backlog.
pub(super) fn resource_throttle_warning(runtime: &OrbitRuntime) -> Option<String> {
    runtime
        .admission_resource_throttle()
        .throttle
        .map(|throttle| throttle.hold_reason())
}

fn execute_stop(runtime: &OrbitRuntime, claim_token: Option<&str>) -> CommandOut {
    let result = runtime.stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
        actor: "cli",
        source: "run_auto_stop",
        reason: None,
        claim_token,
        force: false,
    })?;
    let doc = json!({
        "outcome": result.outcome,
        "pull_settlements": super::support::pull_settlements_json(&result.pull_settlements),
        "coordinators": result.coordinators.iter().map(|change| json!({
            "run_id": change.run_id,
            "job_id": change.job_id,
            "outcome": change.outcome,
            "remaining_children": change.remaining_children.iter().map(|child| json!({
                "run_id": child.run_id,
                "job_name": child.job_name,
                "phase": child.phase,
                "child_status": child.child_status,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    let settlement_lines = super::support::pull_settlement_lines(&result.pull_settlements);
    if result.coordinators.is_empty() {
        let replica = result.replica_owner_machine_id.is_some();
        let mut lines = vec![
            if replica {
                "No active pull drain in this replica checkout."
            } else {
                "No active auto coordinator in this workspace."
            }
            .to_string(),
        ];
        if replica && settlement_lines.is_empty() {
            // The settle-only pass still ran; saying it found nothing tells an
            // operator flushing leftovers that there is nothing left.
            lines.push("No pull settlements pending.".to_string());
        }
        lines.extend(settlement_lines);
        return Ok(Payload::detail(doc, lines.join("\n")).into());
    }
    let mut lines = Vec::new();
    for change in &result.coordinators {
        match change.outcome {
            "cancelled_queued" => lines.push(format!(
                "Cancelled queued auto run {} before it started; it had not admitted any work.",
                change.run_id
            )),
            "unchanged" => lines.push(format!(
                "job run {} already has admissions stopped.",
                change.run_id
            )),
            _ => lines.push(format!(
                "Stopped admissions for job run {} ({}).",
                change.run_id, change.job_id
            )),
        }
        if change.remaining_children.is_empty() {
            if change.outcome != "cancelled_queued" {
                lines.push("No remaining children.".to_string());
            }
        } else {
            lines.push(
                "Remaining children (still running under their existing completion authority):"
                    .to_string(),
            );
            for child in &change.remaining_children {
                let status = child.child_status.as_deref().unwrap_or("-");
                lines.push(format!(
                    "  {} job={} phase={} status={}",
                    child.run_id, child.job_name, child.phase, status
                ));
            }
            lines.push(if change.job_id == orbit_core::application::distributed::PULL_DRAIN_JOB {
                format!(
                    "Claimed leaves finish and settle on their own. `orbit run cancel {} --confirm` \
                     waits for them and then ends the drain; add `--force` to stop them and return \
                     their tasks to the owner's backlog.",
                    change.run_id
                )
            } else {
                "To cancel already-running workers, use `orbit run cancel <run_id> --confirm` on each child."
                    .to_string()
            });
        }
    }
    lines.extend(settlement_lines);
    Ok(Payload::detail(doc, lines.join("\n")).into())
}
