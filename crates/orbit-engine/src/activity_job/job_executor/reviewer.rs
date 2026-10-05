//! [ORB-13890] Before-PR review is charged by reviewer process runtime.
//!
//! The reviewer step's dispatch is the reviewer process. Its start and end
//! are reported to the host around every dispatch — each retry and the
//! post-recovery re-attempt included — so retry backoff, recovery
//! activities and the gate's own steps never count against the lineage's
//! review minutes.
//!
//! [ORB-13992] `review.minutes` is the wall-clock limit for one candidate's
//! review: the host answers a start with what the review has left, and the
//! reviewer process is bounded by it.

use std::time::Instant;

use orbit_types::workflow::ReviewerInvocationEvent;
use orbit_types::workflow::activity_job::{ActivityV2Spec, TargetStep};
use serde_json::Value;

use super::ExecCtx;
use crate::activity_job::cli_runner::DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS;
use crate::context::ReviewerInvocationRequest;

/// The catalog activity a before-PR reviewer runs as.
const REVIEWER_ACTIVITY: &str = "agent_review_repair";

/// A reviewer invocation in flight, reported finished on [`Self::finish`].
pub(super) struct ReviewerInvocation {
    request: ReviewerInvocationRequest,
    started: Instant,
    /// Seconds the host allows this invocation, when it bounds reviews.
    bound_seconds: Option<u64>,
}

impl ReviewerInvocation {
    /// Report the start of the reviewer about to be dispatched, when `target`
    /// is the reviewer activity bound to an admitted attempt.
    pub(super) fn start(
        ctx: &ExecCtx<'_>,
        target: &TargetStep,
        spec: &ActivityV2Spec,
        input: &Value,
    ) -> Option<Self> {
        if target.activity_name.as_deref() != Some(REVIEWER_ACTIVITY) {
            return None;
        }
        let field = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
        };
        let request = ReviewerInvocationRequest {
            run_id: ctx.run_id.clone(),
            lineage_key: field("lineage_key")?,
            attempt_id: field("attempt_id")?,
            event: ReviewerInvocationEvent::Started {
                timeout_seconds: timeout_seconds(target, spec),
            },
        };
        let bound_seconds = record(ctx, &request);
        Some(Self {
            request,
            started: Instant::now(),
            bound_seconds,
        })
    }

    /// Whether the review's minutes were already spent: dispatching would
    /// start a reviewer with no time to run.
    pub(super) fn exhausted(&self) -> bool {
        self.bound_seconds == Some(0)
    }

    /// `spec` shortened to the review's remaining minutes, when that is
    /// tighter than the activity's own wall-clock bound.
    pub(super) fn bounded_spec(&self, spec: &ActivityV2Spec) -> Option<ActivityV2Spec> {
        let bound = self.bound_seconds.filter(|bound| *bound > 0)?;
        match spec {
            ActivityV2Spec::AgentLoop(agent)
                if agent.wall_clock_timeout_seconds == 0
                    || bound < agent.wall_clock_timeout_seconds =>
            {
                let mut agent = agent.clone();
                agent.wall_clock_timeout_seconds = bound;
                Some(ActivityV2Spec::AgentLoop(agent))
            }
            _ => None,
        }
    }

    /// Report the reviewer's end with the runtime it actually took, whether
    /// it succeeded, failed or timed out.
    pub(super) fn finish(mut self, ctx: &ExecCtx<'_>) {
        self.request.event = ReviewerInvocationEvent::Finished {
            runtime_seconds: self.started.elapsed().as_secs(),
        };
        let _ = record(ctx, &self.request);
    }
}

/// The reviewer's own wall-clock bound: no process outlives it, so a start
/// whose end was never reported is never charged past it.
fn timeout_seconds(target: &TargetStep, spec: &ActivityV2Spec) -> u64 {
    match spec {
        ActivityV2Spec::AgentLoop(agent) if agent.wall_clock_timeout_seconds > 0 => {
            agent.wall_clock_timeout_seconds
        }
        _ if target.timeout_seconds > 0 => target.timeout_seconds,
        _ => DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS,
    }
}

/// Charging is evidence, not the reviewer's work: a failed write is logged
/// and the lineage falls back to bounding the invocation by its deadline.
fn record(ctx: &ExecCtx<'_>, request: &ReviewerInvocationRequest) -> Option<u64> {
    ctx.host
        .record_reviewer_invocation(request)
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %request.run_id,
                attempt_id = %request.attempt_id,
                error = %error,
                "could not record the reviewer invocation's runtime"
            );
            None
        })
}
