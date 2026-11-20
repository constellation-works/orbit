//! Standing drain phase 2 (ORB-25991): the opt-in keeper.
//!
//! Once per clock tick, on the workspace owner, the keeper submits one
//! bounded `workspace_auto_pipeline` drain through the same submission path as
//! `orbit run auto`, and only when every condition holds:
//!
//! 1. `workflow.standing_drain.enabled` is true (the default is false) and the
//!    recorded drain intent is `on`. A paused or never-recorded intent is
//!    never acted on: only a hand start lifts a pause.
//! 2. This checkout is the workspace owner and writes its own store.
//! 3. No drain of this workspace is pending or running, and the newest one
//!    is not `held` (a held run is resumable, and resuming it is a person's
//!    or the clock's upgrade resume's decision).
//! 4. Work is waiting: a `backlog` task a drain may admit, or a `proposed`
//!    task it may pilot. This is the waiting set the idle-with-work figure
//!    counts, so it is a ceiling; the drain's own admission ladder decides.
//! 5. `min_resubmit_interval` has passed since the newest drain ended.
//! 6. The crash breaker is closed: fewer than `max_failed_resubmits` keeper
//!    drains ended failed or interrupted in the last hour since a person last
//!    started a drain. An upgrade interruption is a deploy, not a crash, and
//!    does not count.
//!
//! The keeper admits no task and adds no dispatch path: the drain it submits
//! goes through the unchanged admission ladder. Its flags come from config,
//! never from the last run. The submission is recorded as the keeper's (a
//! routine-kind trigger naming [`STANDING_DRAIN_KEEPER`], actor the same), so
//! it records no drain intent. Every submission leaves a
//! [`KEEPER_SUBMIT_AUDIT`] event with the effective flags; a skip leaves a
//! [`KEEPER_SKIP_AUDIT`] event with its reason when it differs from the
//! keeper's previous decision, so an unchanged reason is recorded once rather
//! than on every tick. With the keeper off nothing is read or written.

use chrono::{DateTime, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_config::StandingDrainSettings;
use orbit_store::contracts::{
    AuditEventFilter, DrainIntentRecord, DrainIntentState, JobRunOrder, JobRunQuery,
};
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRunState, JobRunTrigger, WORKSPACE_AUTO_PIPELINE_JOB};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::AutoDrainRequest;
use crate::application::workflow::CompletionPolicy;

/// The keeper's identity: the routine name its drains' trigger records and
/// the actor its submissions and audit events carry.
pub const STANDING_DRAIN_KEEPER: &str = "standing-drain-keeper";
/// Audit tool name of a keeper submission.
pub(crate) const KEEPER_SUBMIT_AUDIT: &str = "drain.keeper.submit";
/// Audit tool name of a keeper skip.
pub(crate) const KEEPER_SKIP_AUDIT: &str = "drain.keeper.skip";

/// The rolling window the crash breaker counts failed keeper drains in.
const BREAKER_WINDOW: TimeDelta = TimeDelta::hours(1);
/// Failed keeper drains read per breaker check; above any admitted threshold.
const BREAKER_SCAN_LIMIT: usize = 128;

/// One keeper decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperDecision {
    /// A drain was submitted.
    Submitted { run_id: String },
    /// Nothing was submitted, for `reason`.
    Skipped { reason: &'static str },
}

/// The crash breaker's reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeeperBreaker {
    /// Keeper drains that ended failed or interrupted inside the window.
    pub failed: usize,
    /// `workflow.standing_drain.max_failed_resubmits`.
    pub threshold: u32,
    /// The newest such failure.
    pub last_failed_at: Option<DateTime<Utc>>,
}

impl KeeperBreaker {
    #[must_use]
    pub fn open(&self) -> bool {
        self.failed >= self.threshold as usize
    }
}

/// The keeper's most recent recorded decision.
#[derive(Debug, Clone, PartialEq)]
pub struct KeeperLastDecision {
    /// `submit` or `skip`.
    pub decision: &'static str,
    pub at: DateTime<Utc>,
    /// The skip reason, or the submitted run id.
    pub detail: Option<String>,
}

/// What readiness, doctor and attention report about the keeper.
#[derive(Debug, Clone, PartialEq)]
pub struct StandingDrainKeeperView {
    pub settings: StandingDrainSettings,
    /// Read only while the keeper is enabled.
    pub breaker: Option<KeeperBreaker>,
    /// Read only while the keeper is enabled.
    pub last_decision: Option<KeeperLastDecision>,
}

impl StandingDrainKeeperView {
    /// The flags each keeper drain carries, as `drain.keeper.submit` records.
    #[must_use]
    pub fn flags_json(&self) -> Value {
        keeper_flags_json(&self.settings)
    }

    /// The readiness projection: `{"enabled": false}` while the keeper is off.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut value = json!({ "enabled": self.settings.enabled });
        if self.settings.enabled {
            value["flags"] = self.flags_json();
            value["min_resubmit_interval_seconds"] =
                json!(self.settings.min_resubmit_interval_seconds);
            value["breaker"] = self.breaker.map_or(Value::Null, |breaker| {
                json!({
                    "open": breaker.open(),
                    "failed_in_last_hour": breaker.failed,
                    "threshold": breaker.threshold,
                })
            });
            value["last_decision"] = self.last_decision.as_ref().map_or(Value::Null, |last| {
                json!({
                    "decision": last.decision,
                    "at": last.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "detail": last.detail,
                })
            });
        }
        value
    }
}

fn keeper_flags_json(settings: &StandingDrainSettings) -> Value {
    json!({
        "window_seconds": settings.window_seconds,
        "concurrency": settings.concurrency,
        "complete": settings.complete,
        "approve_proposed": settings.approve_proposed,
        "auto_fix": settings.auto_fix,
    })
}

impl OrbitRuntime {
    /// One clock tick of the keeper for this workspace. `None` when the keeper
    /// is off: nothing was read, decided or recorded.
    pub(crate) fn run_standing_drain_keeper_tick(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<KeeperDecision>, OrbitError> {
        let settings = self.current_standing_drain();
        if !settings.enabled {
            return Ok(None);
        }
        // A follower's store is not the workspace's; it neither submits nor
        // records a keeper decision.
        if self.is_write_free()
            || self.coordination_write_owner().is_some()
            || self.replica_owner_machine().is_some()
        {
            return Ok(Some(KeeperDecision::Skipped {
                reason: "not_owner",
            }));
        }
        let intent = self.workspace_drain_intent()?;
        if let Some(reason) = self.standing_drain_keeper_skip(&settings, intent.as_ref(), now)? {
            self.record_keeper_skip(reason, &settings, AuditEventStatus::Success, None);
            return Ok(Some(KeeperDecision::Skipped { reason }));
        }
        let completion = if settings.complete {
            CompletionPolicy::Done
        } else {
            CompletionPolicy::Review
        };
        let submitted = self.submit_workspace_auto_run(AutoDrainRequest {
            for_seconds: Some(settings.window_seconds),
            max_active_leaf_runs: settings.concurrency,
            completion,
            actor: Some(STANDING_DRAIN_KEEPER),
            trigger: JobRunTrigger::state_routine(STANDING_DRAIN_KEEPER, STANDING_DRAIN_KEEPER),
            approve_proposed: settings.approve_proposed,
            auto_fix: settings.auto_fix,
            ..AutoDrainRequest::default()
        });
        match submitted {
            Ok(invoke) => {
                let mut arguments = keeper_flags_json(&settings);
                arguments["run_id"] = json!(invoke.run_id);
                self.record_keeper_audit(
                    KEEPER_SUBMIT_AUDIT,
                    Some(&invoke.run_id),
                    AuditEventStatus::Success,
                    arguments,
                    None,
                );
                Ok(Some(KeeperDecision::Submitted {
                    run_id: invoke.run_id,
                }))
            }
            Err(error) => {
                self.record_keeper_skip(
                    "submit_refused",
                    &settings,
                    AuditEventStatus::Failure,
                    Some(error.to_string()),
                );
                Err(error)
            }
        }
    }

    /// Why the keeper would not submit now, or `None` when it would.
    fn standing_drain_keeper_skip(
        &self,
        settings: &StandingDrainSettings,
        intent: Option<&DrainIntentRecord>,
        now: DateTime<Utc>,
    ) -> Result<Option<&'static str>, OrbitError> {
        let Some(intent) = intent else {
            return Ok(Some("intent_off"));
        };
        if intent.intent == DrainIntentState::Paused {
            return Ok(Some("intent_paused"));
        }
        let jobs = self.stores().jobs();
        if !jobs
            .list_pending_or_running_job_runs(WORKSPACE_AUTO_PIPELINE_JOB)?
            .is_empty()
        {
            return Ok(Some("drain_live"));
        }
        let newest = jobs
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(WORKSPACE_AUTO_PIPELINE_JOB.to_string()),
                order_by: JobRunOrder::Recency,
                limit: Some(1),
                include_steps: false,
                input_keys: Some(&[]),
                ..JobRunQuery::default()
            })?
            .into_iter()
            .next();
        if let Some(newest) = &newest {
            if newest.state == JobRunState::Held {
                return Ok(Some("drain_held"));
            }
            let ended = newest.finished_at.unwrap_or(newest.created_at);
            let interval = TimeDelta::seconds(
                i64::try_from(settings.min_resubmit_interval_seconds).unwrap_or(i64::MAX),
            );
            if now.signed_duration_since(ended) < interval {
                return Ok(Some("min_resubmit_interval"));
            }
        }
        if !self.standing_drain_work_waiting()? {
            return Ok(Some("no_work"));
        }
        if self
            .standing_drain_keeper_breaker(settings, Some(intent), now)?
            .open()
        {
            return Ok(Some("breaker_open"));
        }
        Ok(None)
    }

    /// Whether a `backlog` or `proposed` task is waiting.
    pub(crate) fn standing_drain_work_waiting(&self) -> Result<bool, OrbitError> {
        Ok(self
            .list_task_metadata()?
            .iter()
            .any(|task| matches!(task.status, TaskStatus::Backlog | TaskStatus::Proposed)))
    }

    /// The crash breaker: keeper drains that ended failed or interrupted in
    /// the last hour, after the hand start that set the intent `on` (a person
    /// starting a drain closes the breaker). Upgrade interruptions are
    /// deploys and do not count.
    pub(crate) fn standing_drain_keeper_breaker(
        &self,
        settings: &StandingDrainSettings,
        intent: Option<&DrainIntentRecord>,
        now: DateTime<Utc>,
    ) -> Result<KeeperBreaker, OrbitError> {
        let since = now - BREAKER_WINDOW;
        let hand_start = intent
            .filter(|intent| intent.intent == DrainIntentState::On)
            .map(|intent| intent.updated_at);
        let failed = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(WORKSPACE_AUTO_PIPELINE_JOB.to_string()),
            trigger_routine: Some(STANDING_DRAIN_KEEPER.to_string()),
            states: vec![JobRunState::Failed, JobRunState::Interrupted],
            recency_since: Some(since),
            order_by: JobRunOrder::Recency,
            limit: Some(BREAKER_SCAN_LIMIT),
            include_steps: true,
            input_keys: Some(&[]),
            ..JobRunQuery::default()
        })?;
        let mut breaker = KeeperBreaker {
            failed: 0,
            threshold: settings.max_failed_resubmits,
            last_failed_at: None,
        };
        for run in failed {
            // A drain created before the latest hand start belongs to the
            // breaker that start closed.
            if hand_start.is_some_and(|started| run.created_at < started) {
                continue;
            }
            if super::upgrade_resume::upgrade_interrupted(&run) {
                continue;
            }
            let Some(ended) = run.finished_at.filter(|ended| *ended >= since) else {
                continue;
            };
            breaker.failed += 1;
            breaker.last_failed_at = breaker.last_failed_at.max(Some(ended));
        }
        Ok(breaker)
    }

    /// The keeper's configuration, breaker and last decision, for readiness,
    /// doctor and attention. With the keeper off only its settings are read,
    /// so the frequently polled readiness read costs nothing more.
    pub fn standing_drain_keeper_view(&self) -> Result<StandingDrainKeeperView, OrbitError> {
        let settings = self.current_standing_drain();
        if !settings.enabled {
            return Ok(StandingDrainKeeperView {
                settings,
                breaker: None,
                last_decision: None,
            });
        }
        let intent = self.workspace_drain_intent()?;
        Ok(StandingDrainKeeperView {
            settings,
            breaker: Some(self.standing_drain_keeper_breaker(
                &settings,
                intent.as_ref(),
                Utc::now(),
            )?),
            last_decision: self.standing_drain_keeper_last_decision()?,
        })
    }

    /// The newer of the keeper's last submission and last skip.
    fn standing_drain_keeper_last_decision(
        &self,
    ) -> Result<Option<KeeperLastDecision>, OrbitError> {
        let workspace_id = self.workspace_id()?;
        let latest = |tool: &str| {
            self.stores()
                .audit_events()
                .list_audit_events(&AuditEventFilter {
                    tool_name: Some(tool.to_string()),
                    workspace_id: Some(workspace_id.clone()),
                    limit: 1,
                    ..AuditEventFilter::default()
                })
                .map(|events| events.into_iter().next())
        };
        let submit = latest(KEEPER_SUBMIT_AUDIT)?.map(|event| KeeperLastDecision {
            decision: "submit",
            at: event.timestamp,
            detail: event.target_id,
        });
        let skip = latest(KEEPER_SKIP_AUDIT)?.map(|event| KeeperLastDecision {
            decision: "skip",
            at: event.timestamp,
            detail: event
                .arguments_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|arguments| arguments["reason"].as_str().map(str::to_string)),
        });
        Ok(match (submit, skip) {
            (Some(submit), Some(skip)) => Some(if skip.at > submit.at { skip } else { submit }),
            (submit, skip) => submit.or(skip),
        })
    }

    /// Record a skip unless the keeper's last decision was a skip for the
    /// same reason (a refused submission included), so an unchanged reason
    /// writes one row rather than one per tick.
    fn record_keeper_skip(
        &self,
        reason: &'static str,
        settings: &StandingDrainSettings,
        status: AuditEventStatus,
        error: Option<String>,
    ) {
        match self.standing_drain_keeper_last_decision() {
            Ok(Some(last)) if last.decision == "skip" && last.detail.as_deref() == Some(reason) => {
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "standing drain keeper: last decision unreadable");
            }
        }
        let mut arguments = keeper_flags_json(settings);
        arguments["reason"] = json!(reason);
        self.record_keeper_audit(KEEPER_SKIP_AUDIT, None, status, arguments, error);
    }

    fn record_keeper_audit(
        &self,
        tool: &str,
        run_id: Option<&str>,
        status: AuditEventStatus,
        arguments: Value,
        error: Option<String>,
    ) {
        super::log_best_effort(
            "record standing drain keeper audit",
            run_id.unwrap_or(STANDING_DRAIN_KEEPER),
            self.record_workspace_pipeline_audit(super::run::audit_record::PipelineAuditRow {
                tool_name: tool,
                target_id: run_id,
                actor: Some(STANDING_DRAIN_KEEPER),
                status,
                arguments,
                error_message: error,
                workspace_id: None,
            }),
        );
    }
}
