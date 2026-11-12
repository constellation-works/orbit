//! `orbit run extend` [ORB-15383]: a live pull or owner drain's window grows
//! in place instead of lapsing into a tail nothing refills. The extension is
//! read by the drain's own window answer and by readiness, never shortens a
//! window, never reopens stopped admissions or a single-pass drain, and every
//! request is audited against the run with the old and new deadline.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_core::application::job::{DrainWindowExtensionOutcome, DrainWorkerLimitOutcome};
use orbit_core::{DrainWindowExtensionRequest, DrainWorkerLimitRequest, OrbitError, OrbitRuntime};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{AuditEventFilter, JobRunStoreBackend};
use orbit_tools::ToolContext;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tempfile::TempDir;

const PULL: &str = "workspace_pull_pipeline";
const OWNER: &str = "workspace_auto_pipeline";
const COMPLETED_AUDIT: &str = "pipeline.run.window_extend.completed";
const EXTEND_REQUEST_AUDIT: &str = "pipeline.run.window_extend.requested";
const WORKERS_REQUEST_AUDIT: &str = "pipeline.run.workers.requested";
const WORKERS_COMPLETED_AUDIT: &str = "pipeline.run.workers.completed";

struct Drains {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Drains {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            _root: root,
            runtime,
            jobs,
        }
    }

    /// A running drain of `job` submitted with `for_seconds`, whose window
    /// step stamped `stamped`.
    fn running(
        &self,
        job: &str,
        for_seconds: u64,
        stamped: DateTime<Utc>,
        seed: impl FnOnce(&mut PipelineState),
    ) -> String {
        let input = json!({"for_seconds": for_seconds, "max_active_leaf_runs": 2});
        let run = self
            .jobs
            .insert_job_run(job, 1, Utc::now(), Some(input.clone()), None)
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), job.to_string(), input);
        state.record_pipeline_output("open_window", json!({"deadline": stamp(stamped)}));
        seed(&mut state);
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    fn extend(
        &self,
        run_id: &str,
        window: Duration,
        concurrency: Option<u32>,
    ) -> Result<orbit_core::application::job::DrainWindowExtensionChange, OrbitError> {
        self.runtime
            .extend_drain_window(DrainWindowExtensionRequest {
                run_id,
                for_seconds: window.num_seconds() as u64,
                max_active_leaf_runs: concurrency,
                reason: Some("overnight backlog"),
                actor: "operator",
                source: "fixture",
                claim_token: None,
            })
    }

    /// The drain's own window answer for the deadline its loop echoes.
    fn window(&self, run_id: &str, echoed: DateTime<Utc>) -> Value {
        self.runtime
            .run_deterministic(
                "drain_window",
                &json!({}),
                &json!({"deadline": stamp(echoed), "run_id": run_id}),
                ToolContext::default(),
            )
            .unwrap()
    }

    fn state(&self, run_id: &str) -> PipelineState {
        self.runtime.read_run_state(run_id).unwrap().unwrap()
    }

    /// Completion audits recorded against `run_id`, newest first.
    fn completions(&self, run_id: &str, status: AuditEventStatus) -> Vec<Value> {
        self.audits(run_id, COMPLETED_AUDIT, status)
    }

    /// Audit arguments for `tool_name` recorded against `run_id`, newest first.
    fn audits(&self, run_id: &str, tool_name: &str, status: AuditEventStatus) -> Vec<Value> {
        self.runtime
            .list_audit_events_filtered(&AuditEventFilter {
                tool_name: Some(tool_name.to_string()),
                status: Some(status),
                job_run_id: Some(run_id.to_string()),
                limit: 10,
                ..AuditEventFilter::default()
            })
            .unwrap()
            .iter()
            .map(|event| serde_json::from_str(event.arguments_json.as_deref().unwrap()).unwrap())
            .collect()
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn whole_seconds(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&stamp(at))
        .unwrap()
        .with_timezone(&Utc)
}

fn extending_a_pull_or_owner_drain_moves_the_deadline_its_window_reads_and_never_shortens_it() {
    let drains = Drains::new();
    for job in [PULL, OWNER] {
        let stamped = whole_seconds(Utc::now() + Duration::minutes(10));
        let drain = drains.running(job, 3600, stamped, |_| {});

        let before = Utc::now();
        let change = drains.extend(&drain, Duration::hours(2), None).unwrap();
        assert_eq!(
            change.outcome,
            DrainWindowExtensionOutcome::Extended,
            "{job}"
        );
        assert_eq!(change.previous_deadline, stamped, "{job}");
        assert!(
            change.deadline >= before + Duration::hours(2)
                && change.deadline <= Utc::now() + Duration::hours(2),
            "{job}: the deadline becomes now plus the window: {change:?}"
        );
        assert_eq!(change.revision, 1);
        assert!(change.worker_limit.is_none());

        // The loop keeps echoing the stamp; the answer reports the extension.
        let answer = drains.window(&drain, stamped);
        assert_eq!(
            answer["deadline"],
            stamp(change.deadline),
            "{job}: {answer}"
        );
        assert_eq!(answer["expired"], false, "{job}: {answer}");

        let audits = drains.completions(&drain, AuditEventStatus::Success);
        assert_eq!(audits.len(), 1, "{job}: {audits:?}");
        assert_eq!(audits[0]["outcome"], "extended");
        assert_eq!(audits[0]["previous_deadline"], stamp(stamped));
        assert_eq!(audits[0]["deadline"], stamp(change.deadline));

        // A window ending before the one in force changes nothing, and says so.
        let shorter = drains.extend(&drain, Duration::minutes(1), None).unwrap();
        assert_eq!(
            shorter.outcome,
            DrainWindowExtensionOutcome::Unchanged,
            "{job}"
        );
        assert_eq!(shorter.previous_deadline, change.deadline);
        assert_eq!(shorter.deadline, change.deadline);
        assert_eq!(shorter.revision, 1, "a no-op consumes no revision");
        assert_eq!(
            drains
                .state(&drain)
                .pull_drain
                .drain_window_extension
                .unwrap()
                .deadline,
            change.deadline
        );
        let audits = drains.completions(&drain, AuditEventStatus::Success);
        assert_eq!(audits.len(), 2, "{job}: the no-op is audited too");
        assert_eq!(audits[0]["outcome"], "unchanged");
    }

    // Readiness reports the owner drain's extended deadline, not the stamp.
    let readiness = drains
        .runtime
        .workspace_auto_readiness(&[], None, 10, &[])
        .unwrap();
    let extended = drains
        .jobs
        .list_job_runs(OWNER)
        .unwrap()
        .into_iter()
        .map(|run| drains.state(&run.run_id))
        .find_map(|state| state.pull_drain.drain_window_extension)
        .unwrap();
    assert_eq!(
        readiness["capacity"]["ends_at"],
        stamp(extended.deadline),
        "{readiness:#}"
    );
}

fn a_pull_drain_in_its_settling_tail_resumes_requesting_while_a_closed_owner_drain_refuses() {
    let drains = Drains::new();
    let lapsed = whole_seconds(Utc::now() - Duration::minutes(5));

    // A pull drain keeps answering its window until its leaves settle.
    let pull = drains.running(PULL, 3600, lapsed, |_| {});
    assert_eq!(drains.window(&pull, lapsed)["expired"], true);
    let change = drains.extend(&pull, Duration::hours(1), None).unwrap();
    assert_eq!(change.outcome, DrainWindowExtensionOutcome::Extended);
    assert_eq!(change.previous_deadline, lapsed);
    let answer = drains.window(&pull, lapsed);
    assert_eq!(answer["expired"], false, "{answer}");
    assert_eq!(answer["expired_reason"], "open", "{answer}");

    // An owner drain left its loop on the first expired answer.
    let owner = drains.running(OWNER, 3600, lapsed, |_| {});
    let error = drains
        .extend(&owner, Duration::hours(1), Some(4))
        .unwrap_err()
        .to_string();
    assert!(error.contains("closed its window"), "{error}");
    let state = drains.state(&owner);
    assert!(state.pull_drain.drain_window_extension.is_none());
    assert!(
        state.pull_drain.drain_worker_limit.is_none(),
        "a refused extension retunes nothing"
    );
}

fn concurrency_given_with_an_extension_retunes_the_drain_like_run_concurrency() {
    let drains = Drains::new();
    for job in [PULL, OWNER] {
        let drain = drains.running(job, 3600, Utc::now() + Duration::minutes(10), |_| {});
        let change = drains.extend(&drain, Duration::hours(4), Some(5)).unwrap();
        assert_eq!(change.outcome, DrainWindowExtensionOutcome::Extended);
        let limit = change.worker_limit.expect("the ceiling was retuned");
        assert_eq!(limit.outcome, DrainWorkerLimitOutcome::Updated);
        assert_eq!(limit.previous_max_active_leaf_runs, 2);
        assert_eq!(limit.max_active_leaf_runs, 5);
        let state = drains.state(&drain);
        let persisted = state.pull_drain.drain_worker_limit.unwrap();
        assert_eq!(persisted.max_active_leaf_runs, 5);
        assert_eq!(persisted.actor, "operator");
        assert_eq!(
            state.pull_drain.drain_window_extension.unwrap().deadline,
            change.deadline
        );

        // A retune alongside a no-op extension still applies.
        let again = drains
            .extend(&drain, Duration::minutes(1), Some(1))
            .unwrap();
        assert_eq!(again.outcome, DrainWindowExtensionOutcome::Unchanged);
        assert_eq!(again.worker_limit.unwrap().max_active_leaf_runs, 1);
    }
}

fn stopped_cancelling_single_pass_and_ineligible_runs_refuse_the_extension_and_stay_closed() {
    let drains = Drains::new();
    let open = Utc::now() + Duration::minutes(10);
    for job in [PULL, OWNER] {
        let stopped = drains.running(job, 3600, open, |state| {
            state.set_drain_admissions_stop("operator".into(), Some("on-call".into()));
        });
        let error = drains
            .extend(&stopped, Duration::hours(1), Some(4))
            .unwrap_err()
            .to_string();
        assert!(error.contains("admissions stopped"), "{job}: {error}");
        let answer = drains.window(&stopped, open);
        assert_eq!(answer["expired"], true, "{job}: the stop still holds");
        assert_eq!(answer["expired_reason"], "admissions_stopped");
        let state = drains.state(&stopped);
        assert!(state.pull_drain.drain_window_extension.is_none());
        assert!(state.pull_drain.drain_worker_limit.is_none());

        let cancelling = drains.running(job, 3600, open, |state| {
            state.set_drain_cancel("operator".into(), "fixture".into(), None);
        });
        let error = drains
            .extend(&cancelling, Duration::hours(1), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("being cancelled"), "{job}: {error}");

        // `--for` omitted: one admission pass, so no window to extend.
        let single_pass = drains.running(job, 0, Utc::now(), |_| {});
        let error = drains
            .extend(&single_pass, Duration::hours(1), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("single-pass mode"), "{job}: {error}");
        assert!(
            drains
                .state(&single_pass)
                .pull_drain
                .drain_window_extension
                .is_none()
        );
        let rejected = drains.completions(&single_pass, AuditEventStatus::Failure);
        assert_eq!(rejected.len(), 1, "{job}: {rejected:?}");
        assert_eq!(rejected[0]["outcome"], "rejected");
    }

    // A finished drain, a queued one that never opened its window, and a run
    // that is not a drain.
    let finished = drains.running(PULL, 3600, open, |_| {});
    drains
        .jobs
        .finalize_job_run(&finished, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let error = drains
        .extend(&finished, Duration::hours(1), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("terminal run"), "{error}");

    let queued = drains
        .jobs
        .insert_job_run(
            OWNER,
            1,
            Utc::now(),
            Some(json!({"for_seconds": 3600})),
            None,
        )
        .unwrap();
    let error = drains
        .extend(&queued.run_id, Duration::hours(1), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("has not started"), "{error}");

    let leaf = drains
        .jobs
        .insert_job_run("task_auto_pipeline", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    let error = drains
        .extend(&leaf.run_id, Duration::hours(1), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("window to extend"), "{error}");
}

fn invalid_concurrency_given_with_an_extension_fails_and_leaves_state_unchanged() {
    let drains = Drains::new();
    for job in [PULL, OWNER] {
        let stamped = whole_seconds(Utc::now() + Duration::minutes(10));
        let drain = drains.running(job, 3600, stamped, |_| {});
        let before_state = drains.state(&drain);

        for window in [
            Duration::zero(),
            Duration::seconds(86_401),
            Duration::days(2),
        ] {
            let error = drains.extend(&drain, window, None).unwrap_err();
            assert!(
                matches!(error, OrbitError::InvalidInput(ref message)
                if message == "the extension must be between 1 second and 86400 seconds (24 hours)"),
                "{error}"
            );
            assert_eq!(
                drains.state(&drain),
                before_state,
                "{job}: invalid windows change no state"
            );
        }
        let error = drains
            .extend(&drain, Duration::hours(3), Some(0))
            .unwrap_err();
        match error {
            OrbitError::InvalidInput(msg) => {
                assert!(
                    msg.contains("worker ceiling must be at least 1"),
                    "{job}: {msg}"
                );
            }
            other => panic!("{job}: expected InvalidInput, got {other:?}"),
        }

        let after_state = drains.state(&drain);
        assert_eq!(
            after_state, before_state,
            "{job}: state must remain unchanged"
        );
        assert!(
            after_state.pull_drain.drain_window_extension.is_none(),
            "{job}"
        );
        assert!(after_state.pull_drain.drain_worker_limit.is_none(), "{job}");

        let answer = drains.window(&drain, stamped);
        assert_eq!(
            answer["deadline"],
            stamp(stamped),
            "{job}: deadline unchanged"
        );
        assert_eq!(answer["expired"], false, "{job}: drain still open");

        let completions_failure = drains.completions(&drain, AuditEventStatus::Failure);
        assert_eq!(
            completions_failure.len(),
            4,
            "{job}: {completions_failure:?}"
        );
        assert_eq!(completions_failure[0]["outcome"], "rejected");

        let completions_success = drains.completions(&drain, AuditEventStatus::Success);
        assert!(
            completions_success.is_empty(),
            "{job}: no success audit should be recorded"
        );
        let accepted = drains.extend(&drain, Duration::hours(24), None).unwrap();
        assert_eq!(accepted.outcome, DrainWindowExtensionOutcome::Extended);
    }
}

fn acquire_claim(runtime: &OrbitRuntime) {
    use orbit_store::contracts::WorkspaceClaimAcquireParams;
    let store = runtime.sqlite_store().unwrap();
    store
        .acquire_workspace_claim(&WorkspaceClaimAcquireParams {
            workspace_orbit_dir: runtime.paths().orbit_dir.to_string_lossy().into_owned(),
            workspace_id: runtime.workspace_id().ok(),
            actor: "claim-racer".to_string(),
            ttl_seconds: 3600,
            machine_id: Some("claim-racer".to_string()),
            session_id: Some("claim-racer".to_string()),
        })
        .unwrap();
}

/// A claim taken by another operator after the extension would have
/// committed must not leave a combined request half-applied: the request
/// either succeeds with both controls or refuses with neither changed.
fn a_claim_taken_during_a_combined_extension_leaves_no_partial_change() {
    for job in [PULL, OWNER] {
        let drains = Drains::new();
        let stamped = whole_seconds(Utc::now() + Duration::minutes(10));
        let drain = drains.running(job, 3600, stamped, |_| {});
        let racer_runtime = drains.runtime.clone();
        let racer_drain = drain.clone();
        let racer = std::thread::spawn(move || {
            let deadline = Instant::now() + StdDuration::from_secs(10);
            while Instant::now() < deadline {
                let extended = racer_runtime
                    .read_run_state(&racer_drain)
                    .ok()
                    .flatten()
                    .and_then(|state| state.pull_drain.drain_window_extension)
                    .is_some();
                if extended {
                    acquire_claim(&racer_runtime);
                    return;
                }
                std::hint::spin_loop();
            }
        });
        let result = drains.extend(&drain, Duration::hours(2), Some(5));
        let _ = racer.join();
        let state = drains.state(&drain);
        match result {
            Ok(change) => {
                assert_eq!(
                    change.outcome,
                    DrainWindowExtensionOutcome::Extended,
                    "{job}"
                );
                assert_eq!(
                    change
                        .worker_limit
                        .expect("ceiling retuned")
                        .max_active_leaf_runs,
                    5,
                    "{job}"
                );
                assert!(state.pull_drain.drain_window_extension.is_some(), "{job}");
                assert_eq!(
                    state
                        .pull_drain
                        .drain_worker_limit
                        .expect("ceiling persisted")
                        .max_active_leaf_runs,
                    5,
                    "{job}"
                );
            }
            Err(error) => {
                assert!(
                    state.pull_drain.drain_window_extension.is_none(),
                    "{job}: refused combined extension left the longer window behind: {error}"
                );
                assert!(
                    state.pull_drain.drain_worker_limit.is_none(),
                    "{job}: refused combined extension left the ceiling behind: {error}"
                );
            }
        }
    }
}

fn combined_extension_resize_audit_links_to_the_extension_request() {
    let drains = Drains::new();
    for job in [PULL, OWNER] {
        let drain = drains.running(job, 3600, Utc::now() + Duration::minutes(10), |_| {});
        let change = drains.extend(&drain, Duration::hours(4), Some(5)).unwrap();
        assert_eq!(
            change.worker_limit.unwrap().outcome,
            DrainWorkerLimitOutcome::Updated
        );

        let extend_requests =
            drains.audits(&drain, EXTEND_REQUEST_AUDIT, AuditEventStatus::Success);
        assert_eq!(extend_requests.len(), 1, "{job}: {extend_requests:?}");
        let initiating_id = extend_requests[0]["request_id"].as_str().unwrap();
        assert_eq!(extend_requests[0]["actor"], "operator", "{job}");
        assert_eq!(extend_requests[0]["source"], "fixture", "{job}");
        assert_eq!(extend_requests[0]["reason"], "overnight backlog", "{job}");

        let worker_requests =
            drains.audits(&drain, WORKERS_REQUEST_AUDIT, AuditEventStatus::Success);
        assert_eq!(worker_requests.len(), 1, "{job}: {worker_requests:?}");
        assert_eq!(worker_requests[0]["request_id"], initiating_id, "{job}");
        assert_eq!(
            worker_requests[0]["requested_at"], extend_requests[0]["requested_at"],
            "{job}: the resize request carries its extension's request time"
        );
        assert_eq!(
            worker_requests[0]["requested_max_active_leaf_runs"], 5,
            "{job}"
        );
        assert_eq!(worker_requests[0]["actor"], "operator", "{job}");
        assert_eq!(worker_requests[0]["source"], "fixture", "{job}");
        assert_eq!(worker_requests[0]["reason"], "overnight backlog", "{job}");

        let worker_completed =
            drains.audits(&drain, WORKERS_COMPLETED_AUDIT, AuditEventStatus::Success);
        assert_eq!(worker_completed.len(), 1, "{job}: {worker_completed:?}");
        assert_eq!(
            worker_completed[0]["request_id"], initiating_id,
            "{job}: resize completion must join to its initiating extension request"
        );
        assert_eq!(worker_completed[0]["outcome"], "updated", "{job}");
        assert_eq!(worker_completed[0]["max_active_leaf_runs"], 5, "{job}");
        assert_eq!(
            worker_completed[0]["previous_max_active_leaf_runs"], 2,
            "{job}"
        );
        assert_eq!(worker_completed[0]["actor"], "operator", "{job}");
        assert_eq!(worker_completed[0]["source"], "fixture", "{job}");
        assert_eq!(worker_completed[0]["reason"], "overnight backlog", "{job}");

        // A combined request whose ceiling is unchanged still emits linked evidence.
        let again = drains
            .extend(&drain, Duration::minutes(1), Some(5))
            .unwrap();
        assert_eq!(
            again.worker_limit.unwrap().outcome,
            DrainWorkerLimitOutcome::Unchanged
        );
        let extend_requests =
            drains.audits(&drain, EXTEND_REQUEST_AUDIT, AuditEventStatus::Success);
        assert_eq!(extend_requests.len(), 2, "{job}");
        let second_id = extend_requests[0]["request_id"].as_str().unwrap();
        assert_ne!(second_id, initiating_id, "{job}");
        let worker_requests =
            drains.audits(&drain, WORKERS_REQUEST_AUDIT, AuditEventStatus::Success);
        assert_eq!(worker_requests.len(), 2, "{job}: {worker_requests:?}");
        assert_eq!(worker_requests[0]["request_id"], second_id, "{job}");
        assert_eq!(
            worker_requests[0]["requested_at"], extend_requests[0]["requested_at"],
            "{job}"
        );
        let worker_completed =
            drains.audits(&drain, WORKERS_COMPLETED_AUDIT, AuditEventStatus::Success);
        assert_eq!(worker_completed.len(), 2, "{job}: {worker_completed:?}");
        assert_eq!(worker_completed[0]["request_id"], second_id, "{job}");
        assert_eq!(worker_completed[0]["outcome"], "unchanged", "{job}");
        assert_eq!(worker_completed[0]["actor"], "operator", "{job}");
        assert_eq!(worker_completed[0]["source"], "fixture", "{job}");
        assert_eq!(worker_completed[0]["reason"], "overnight backlog", "{job}");
    }

    // Standalone resize pairing remains intact.
    let standalone = drains.running(PULL, 3600, Utc::now() + Duration::minutes(10), |_| {});
    drains
        .runtime
        .set_drain_worker_limit(DrainWorkerLimitRequest {
            run_id: &standalone,
            max_active_leaf_runs: 4,
            expected_revision: None,
            reason: Some("standalone retune"),
            actor: "operator",
            source: "fixture",
            claim_token: None,
        })
        .unwrap();
    let requests = drains.audits(
        &standalone,
        WORKERS_REQUEST_AUDIT,
        AuditEventStatus::Success,
    );
    let completed = drains.audits(
        &standalone,
        WORKERS_COMPLETED_AUDIT,
        AuditEventStatus::Success,
    );
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(completed.len(), 1, "{completed:?}");
    assert_eq!(requests[0]["request_id"], completed[0]["request_id"]);
    assert_eq!(requests[0]["actor"], "operator");
    assert_eq!(requests[0]["source"], "fixture");
    assert_eq!(requests[0]["reason"], "standalone retune");
    assert_eq!(completed[0]["outcome"], "updated");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    extending_a_pull_or_owner_drain_moves_the_deadline_its_window_reads_and_never_shortens_it_scenarios,
    extending_a_pull_or_owner_drain_moves_the_deadline_its_window_reads_and_never_shortens_it,
    a_pull_drain_in_its_settling_tail_resumes_requesting_while_a_closed_owner_drain_refuses,
    concurrency_given_with_an_extension_retunes_the_drain_like_run_concurrency,
    stopped_cancelling_single_pass_and_ineligible_runs_refuse_the_extension_and_stay_closed,
    invalid_concurrency_given_with_an_extension_fails_and_leaves_state_unchanged,
    a_claim_taken_during_a_combined_extension_leaves_no_partial_change,
    combined_extension_resize_audit_links_to_the_extension_request
);
