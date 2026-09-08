//! `orbit run auto` workspace logistics entrypoint.

use clap::Args;
use orbit_core::{
    CompletionPolicy, DrainAdmissionsStopRequest, OperationDrainRequest, OrbitRuntime,
};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_duration_seconds;

use super::support::{WorkflowDispatchResult, workflow_dispatch_payload};

pub(super) const AUTO_WORKFLOW: &str = "auto";

#[derive(Args)]
#[command(
    about = "Drain the workspace backlog for a window (loose leaves, plus one epic)",
    override_usage = "orbit run auto [OPTIONS]",
    after_help = "Examples:\n  orbit run auto\n  orbit run auto --medium-complexity-crews grok,terra\n  orbit run auto --for 4h\n  orbit run auto --for 4h --concurrency 8\n  orbit run auto --for 4h --complete\n  orbit run auto --stop\n\n\
                  The drain re-lists the whole backlog every pass and keeps `--concurrency`\n\
                  tasks in flight, starting a replacement as each one finishes rather than\n\
                  waiting for the batch. An epic root runs alongside the leaves, one at a time.\n\n\
                  `--complete` is blanket authorization: it applies to every task the drain\n\
                  admits for the whole window, including work that reaches the backlog after\n\
                  the run starts. The drain is asynchronous, so this prints the durable run ID\n\
                  and returns without knowing the eventual outcome.\n\n\
                  Complexity pools select only for tasks without an explicit crew.\n\
                  Each CLI pool replaces its matching workflow pool for this drain.\n\
                  Empty pools and unset complexity use the existing default crew chain.\n\
                  Selections are recorded at admission and retained on retries/resume.\n\
                  Pools do not restrict manual crew choices.\n\n\
                  `--allow-crew` restricts this one run to the named crews, for its window\n\
                  only. It edits no configuration and reassigns nothing: a backlog task whose\n\
                  crew is excluded is simply not started, and `orbit run readiness --allow-crew`\n\
                  names it. To actually move that work, reassign its crew yourself. Tasks a\n\
                  different invocation already has in flight keep running.\n\n\
                  `--stop` ends new admissions for this workspace's active auto coordinator.\n\
                  You do not need a run ID. Already admitted workers keep running under the\n\
                  completion authority they were started with; this is not cancellation.\n\
                  To cancel those workers, `orbit run cancel <RUN_ID> --confirm` each child.\n\
                  A second `--stop`, or `--stop` with no active coordinator, is a no-op.\n\n\
                  `--grant <ID>` binds the drain to an operation-mode grant enabled with\n\
                  `orbit operation enable`: the window is capped at the grant's remaining time,\n\
                  only the grant's finite task set is admitted, promotion follows fresh pilot\n\
                  evidence, and completion is the grant's captured authority rather than\n\
                  `--complete`. Each child admission rechecks the grant.\n\n\
                  Inspect submitted runs with `orbit run history -j workspace_auto_pipeline` and\n\
                  `orbit run show <RUN_ID>`."
)]
pub struct AutoCommand {
    /// How long to keep draining, e.g. `30m`, `2h`. Without it the run takes
    /// one tick and stops. The window bounds only the start of new work: a
    /// task already being shipped when it expires still finishes.
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
    /// whole window, not just the backlog visible right now. Off by default,
    /// and it never approves `proposed` work for the backlog.
    #[arg(long)]
    pub complete: bool,
    /// Restrict this run to these configured crews, e.g. when a provider is
    /// unavailable or its budget is spent. Repeatable and comma-separated.
    /// Every name must be configured here; an unknown or empty one fails
    /// before anything is dispatched. Omitted, the drain runs every crew, as
    /// before. This is scoped to this run's window only — no workspace
    /// configuration is changed, no task is reassigned, and nothing another
    /// invocation is already running is cancelled.
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Random crew pool for unassigned low-complexity tasks. Overrides the
    /// matching workflow pool; pass the flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub low_complexity_crews: Option<Vec<String>>,
    /// Random crew pool for unassigned medium-complexity tasks. Overrides the
    /// matching workflow pool; pass the flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub medium_complexity_crews: Option<Vec<String>>,
    /// Random crew pool for unassigned hard-complexity tasks. Overrides the
    /// matching workflow pool; pass the flag with no names to disable it.
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub hard_complexity_crews: Option<Vec<String>>,
    /// Bind this drain to an operation-mode grant (see `orbit operation`).
    /// Completion, scope, and limits come from the grant; `--complete` is
    /// not accepted alongside it.
    #[arg(long, value_name = "GRANT_ID", conflicts_with = "complete")]
    pub grant: Option<String>,
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
        conflicts_with_all = ["for_duration", "concurrency", "complete", "allow_crew", "grant", "low_complexity_crews", "medium_complexity_crews", "hard_complexity_crews"]
    )]
    pub stop: bool,
}

impl Execute for AutoCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.stop {
            return execute_stop(runtime, self.claim_token.as_deref());
        }
        let complexity_crews = orbit_config::ComplexityCrewPools {
            low: self.low_complexity_crews,
            medium: self.medium_complexity_crews,
            hard: self.hard_complexity_crews,
        };
        let for_seconds = self
            .for_duration
            .as_deref()
            .map(parse_duration_seconds)
            .transpose()?;
        if let Some(grant_id) = self.grant.as_deref() {
            return execute_grant_bound(
                runtime,
                grant_id,
                for_seconds,
                self.concurrency,
                &self.allow_crew,
                &complexity_crews,
                self.claim_token.as_deref(),
            );
        }
        let completion = if self.complete {
            CompletionPolicy::Done
        } else {
            CompletionPolicy::Review
        };
        let invoke = runtime.submit_workspace_auto_run(
            for_seconds,
            self.concurrency,
            completion,
            &self.allow_crew,
            &complexity_crews,
            None,
            self.claim_token.as_deref(),
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
        workflow_dispatch_payload(AUTO_WORKFLOW, &[run])
    }
}

/// [ORB-11332] A drain whose every admission is bound to a grant.
#[allow(clippy::too_many_arguments)]
fn execute_grant_bound(
    runtime: &OrbitRuntime,
    grant_id: &str,
    for_seconds: Option<u64>,
    concurrency: Option<u32>,
    allow_crew: &[String],
    complexity_crews: &orbit_config::ComplexityCrewPools,
    claim_token: Option<&str>,
) -> CommandOut {
    let result = runtime.submit_operation_drain(OperationDrainRequest {
        grant_id: Some(grant_id),
        for_seconds,
        max_active_leaf_runs: concurrency,
        allowed_crews: allow_crew,
        complexity_crews,
        actor: None,
        claim_token,
    })?;
    Ok(Payload::detail(
        json!({
            "workflow": AUTO_WORKFLOW,
            "job_id": result.invoke.job_name,
            "run_id": result.invoke.run_id,
            "state": if result.invoke.queued { "queued" } else { "submitted" },
            "grant_id": result.admission.grant_id,
            "grant_revision": result.admission.grant_revision,
            "completion": result.admission.completion,
            "window_seconds": result.window_seconds,
            "leaf_ceiling": result.leaf_ceiling,
            "expires_at": result.admission.expires_at.to_rfc3339(),
        }),
        format!(
            "Submitted auto run {} under grant {} (completion: {}, window: {}s, leaf ceiling: {}).",
            result.invoke.run_id,
            result.admission.grant_id,
            result.admission.completion,
            result.window_seconds,
            result.leaf_ceiling
        ),
    )
    .into())
}

fn execute_stop(runtime: &OrbitRuntime, claim_token: Option<&str>) -> CommandOut {
    let result = runtime.stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
        actor: "cli",
        source: "run_auto_stop",
        reason: None,
        claim_token,
    })?;
    let doc = json!({
        "outcome": result.outcome,
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
    if result.coordinators.is_empty() {
        return Ok(Payload::detail(doc, "No active auto coordinator in this workspace.").into());
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
            lines.push(
                "To cancel already-running workers, use `orbit run cancel <run_id> --confirm` on each child."
                    .to_string(),
            );
        }
    }
    Ok(Payload::detail(doc, lines.join("\n")).into())
}
