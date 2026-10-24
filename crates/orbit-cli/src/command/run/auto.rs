//! `orbit run auto` workspace logistics entrypoint.

use clap::Args;
use orbit_core::application::job::{DrainAdmissionsStopOutcome, SubmissionKind};
use orbit_core::{
    CompletionPolicy, DrainAdmissionsStopRequest, OrbitRuntime, WorkspacePullRequest,
};

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_duration_seconds;

use super::support::{WorkflowDispatchResult, workflow_dispatch_payload_with_notices};

pub(super) const AUTO_WORKFLOW: &str = "auto";

#[derive(Args)]
#[command(
    about = "Drain the workspace backlog for a window",
    override_usage = "orbit run auto [OPTIONS]",
    after_help = "Examples:\n  orbit run auto\n  orbit run auto --medium-complexity-crews grok,sol\n  orbit run auto --for 4h\n  orbit run auto --for 4h --concurrency 8\n  orbit run auto --for 4h --complete\n  orbit run auto --for 4h --auto-fix\n  orbit run auto --for 4h --approve-proposed\n  orbit run auto --for 2h --review before-landing\n  orbit run auto --stop\n  orbit run auto --pull hm_owner/ws_orbit --for 8h --concurrency 3\n  orbit run auto --pull hm_owner/ws_orbit --for 8h --allow-crew sol,luna\n\n\
                  Prints the run ID and returns; follow it with `orbit run show <RUN_ID>`.\n\
                  Run-wide flags cover every task the window admits, even ones filed later.\n\
                  Details: the orbit-orchestrate skill, references/orchestration.md."
)]
pub struct AutoCommand {
    /// How long to keep admitting work, e.g. `30m`, `2h` (default: one pass).
    #[arg(long = "for", value_name = "DURATION")]
    pub for_duration: Option<String>,
    /// Maximum tasks in flight (default 5).
    #[arg(long, value_name = "N")]
    pub concurrency: Option<u32>,
    /// Forecast admission mode (default: `machine.auto_concurrency.mode`).
    #[arg(long, value_name = "MODE", value_parser = ["off", "shadow", "enforce"])]
    pub auto_concurrency: Option<String>,
    /// Land shipped tasks and mark them `done` instead of leaving them in `review`.
    #[arg(long)]
    pub complete: bool,
    /// Approve `proposed` tasks that pass task-pilot verification.
    #[arg(long)]
    pub approve_proposed: bool,
    /// Repair failing checks and conflicts on the pull requests this drain opens.
    #[arg(long)]
    pub auto_fix: bool,
    /// Add a review layer before landing when workspace policy has none.
    #[arg(long, value_enum, value_name = "LAYER")]
    pub review: Option<orbit_types::workflow::ReviewLayer>,
    /// Start only tasks on these crews (repeatable, comma-separated).
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Require a systemd user scope for the coordinator and every worker.
    #[arg(long)]
    pub strict_worker_containment: bool,
    /// Crew pool for unassigned low-complexity tasks (`crew` or `crew:weight`).
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub low_complexity_crews: Option<Vec<String>>,
    /// Crew pool for unassigned medium-complexity tasks (`crew` or `crew:weight`).
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub medium_complexity_crews: Option<Vec<String>>,
    /// Crew pool for unassigned hard-complexity tasks (`crew` or `crew:weight`).
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub hard_complexity_crews: Option<Vec<String>>,
    /// Crew pool for unassigned xhard-complexity tasks (`crew` or `crew:weight`).
    #[arg(long, value_name = "CREW", value_delimiter = ',', num_args = 0..)]
    pub xhard_complexity_crews: Option<Vec<String>>,
    /// Claim work from this owner's backlog and hand results back as pull requests.
    #[arg(
        long,
        value_name = "SELECTOR",
        conflicts_with_all = ["auto_concurrency", "complete", "approve_proposed", "auto_fix", "strict_worker_containment", "low_complexity_crews", "medium_complexity_crews", "hard_complexity_crews", "xhard_complexity_crews", "claim_token"]
    )]
    pub pull: Option<String>,
    /// Owner host for `--pull <workspace>` (name or `machine_id`).
    #[arg(long, value_name = "HOST", requires = "pull")]
    pub host: Option<String>,
    /// Workspace claim token (default: `ORBIT_WORKSPACE_CLAIM_TOKEN`).
    #[arg(long)]
    pub claim_token: Option<String>,
    /// Stop admitting new work; running tasks continue.
    #[arg(
        long,
        conflicts_with_all = ["for_duration", "concurrency", "auto_concurrency", "complete", "approve_proposed", "auto_fix", "review", "allow_crew", "strict_worker_containment", "low_complexity_crews", "medium_complexity_crews", "hard_complexity_crews", "xhard_complexity_crews", "pull"]
    )]
    pub stop: bool,
}

impl Execute for AutoCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.stop {
            return execute_stop(runtime, self.claim_token.as_deref());
        }
        if let Some(selector) = self.pull.as_deref() {
            if self.review.is_some() {
                return Err(orbit_core::OrbitError::InvalidInput(
                    orbit_core::application::review::pull_review_request_refusal().into(),
                ));
            }
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
            let notices = runtime.submission_notices(SubmissionKind::PullDrain);
            let mut warnings = notices.warnings();
            // [ORB-15622] A replica whose gates drifted from the owner's.
            warnings.extend(runtime.pull_gate_drift(&invoke.run_id)?);
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
                    wait_timeout: false,
                    error_code: None,
                    error_message: None,
                }],
                warnings,
                notices.notes(),
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
        let invoke = runtime.submit_workspace_auto_run_with_mode(
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
            self.auto_fix,
            self.auto_concurrency
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|message: &str| orbit_core::OrbitError::InvalidInput(message.into()))?,
            self.review,
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
            wait_timeout: false,
            error_code: None,
            error_message: None,
        };
        let mode = runtime.run_auto_concurrency(&run.run_id)?;
        let notices = runtime.submission_notices(SubmissionKind::Drain);
        super::support::workflow_dispatch_payload_with_auto_concurrency(
            AUTO_WORKFLOW,
            &[run],
            notices.warnings(),
            notices.notes(),
            mode,
        )
    }
}

fn execute_stop(runtime: &OrbitRuntime, claim_token: Option<&str>) -> CommandOut {
    let result = runtime.stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
        actor: "cli",
        source: "run_auto_stop",
        reason: None,
        claim_token,
        force: false,
    })?;
    let doc = result.to_json();
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
            DrainAdmissionsStopOutcome::CancelledQueued => lines.push(format!(
                "Cancelled queued auto run {} before it started; it had not admitted any work.",
                change.run_id
            )),
            DrainAdmissionsStopOutcome::Unchanged => lines.push(format!(
                "job run {} already has admissions stopped.",
                change.run_id
            )),
            _ => lines.push(format!(
                "Stopped admissions for job run {} ({}).",
                change.run_id, change.job_id
            )),
        }
        if change.remaining_children.is_empty() {
            if change.outcome != DrainAdmissionsStopOutcome::CancelledQueued {
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
            lines.push(if change.job_id == orbit_core::application::contracts::PULL_DRAIN_JOB {
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
