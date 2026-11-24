//! [ORB-23003] An owner-local delivery that ends in review (a dashboard
//! ship, `orbit run ship`) has no handoff, so the landing never watched its
//! pull request: a red one sat in review with nothing on the task. The
//! review watch reads its checks and records the landing's classification.
use super::*;
use orbit_types::workflow::handoff::{
    HistoricalLandingChecksObservation, LANDING_CHECKS_OBSERVED_EVENT,
    LandingChecksObservationState,
};

#[test]
fn a_review_delivery_with_failed_checks_records_their_classification() {
    isolated(
        "review_watch::a_review_delivery_with_failed_checks_records_their_classification",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["failure"]);
            action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect("open the delivery's pull request");
            host.set_status(TASK_ID, TaskStatus::Review);
            plant_run(
                &fx,
                &fx.candidate,
                10,
                "failure",
                "error: assertion failed: answer == 42\n",
            );
            plant_run(&fx, &fx.base_sha, 20, "success", "");
            let watch = json!({
                "task_ids": [TASK_ID],
                "pr_number": PR_NUMBER,
                "workspace_path": fx.repo,
                "base": BASE,
                "base_sync": "local",
                "max_wait_seconds": 30,
                "poll_interval_seconds": 5,
            });
            let watched = action(&host, "pr_checks_watch", &watch)
                .expect("the review watch is a shipped action");
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed: Vec<HistoricalLandingChecksObservation> = host
                .history(TASK_ID)
                .iter()
                .filter_map(HistoricalLandingChecksObservation::from_history_entry)
                .collect();
            assert_eq!(observed.len(), 1, "one landing check event: {observed:?}");
            let recorded = &observed[0];
            let note = recorded
                .observation()
                .expect("the recorded note decodes as an observation");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Failed,
                "{note:?}"
            );
            assert_eq!(note.pr_number(), PR_NUMBER, "{note:?}");
            assert_eq!(
                note.classification(),
                Some("candidate"),
                "the landing's classification: green on the fork point, red on the head: {note:?}"
            );
            assert_eq!(
                note.failed_checks(),
                Some(&["test".to_string()][..]),
                "{note:?}"
            );
            // The note keeps the bytes the untyped writer produced, which the
            // dashboard parses and replay compares: this key order, `head_sha`
            // always present, absent optional fields omitted.
            let legacy = serde_json::to_string(&json!({
                "state": "failed",
                "pr_number": note.pr_number(),
                "head_sha": note.head_sha(),
                "failed_checks": note.failed_checks(),
                "classification": note.classification(),
                "detail": note.detail(),
            }))
            .unwrap();
            assert_eq!(
                recorded.replay_key().as_note(),
                legacy,
                "the note's bytes are unchanged"
            );
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::Review,
                "the watch never moves the task"
            );

            // A repeated watch of the same head records nothing new.
            action(&host, "pr_checks_watch", &watch).expect("watch again");
            let again = host
                .history(TASK_ID)
                .iter()
                .filter(|entry| entry.event == LANDING_CHECKS_OBSERVED_EVENT)
                .count();
            assert_eq!(again, 1, "an identical observation is recorded once");
        },
    );
}

const GRAPHQL_OUTAGE: &str = "GraphQL: Something went wrong while executing your query";

fn open_review_delivery(fx: &Fixture, host: &DeliveryHost) {
    action(host, "pr_open", &fx.open_input(&fx.candidate, &fx.base_sha))
        .expect("open the delivery's pull request");
    host.set_status(TASK_ID, TaskStatus::Review);
}

fn watch_input(fx: &Fixture, max_wait_seconds: u64) -> Value {
    json!({
        "task_ids": [TASK_ID],
        "pr_number": PR_NUMBER,
        "workspace_path": fx.repo,
        "base": BASE,
        "base_sync": "local",
        "max_wait_seconds": max_wait_seconds,
        "poll_interval_seconds": 5,
    })
}

fn observations(host: &DeliveryHost) -> Vec<HistoricalLandingChecksObservation> {
    host.history(TASK_ID)
        .iter()
        .filter_map(HistoricalLandingChecksObservation::from_history_entry)
        .collect()
}

/// No call that changes the pull request, its branch or its checks reached the forge.
fn assert_no_mutation(fx: &Fixture) {
    let mutating: Vec<String> = fx
        .forge_calls()
        .into_iter()
        .filter(|call| {
            [
                "pr merge",
                "pr edit",
                "pr close",
                "pr comment",
                "pr ready",
                "run rerun",
            ]
            .iter()
            .any(|verb| call.starts_with(verb))
        })
        .collect();
    assert!(mutating.is_empty(), "the watch mutated: {mutating:?}");
}

/// A status read the provider fails transiently is an unknown observation:
/// the watch keeps polling, then reports what the next answered read shows.
#[test]
fn a_transient_status_read_failure_keeps_the_watch_polling() {
    isolated(
        "review_watch::a_transient_status_read_failure_keeps_the_watch_polling",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            // More failures than one read's own bounded retries can absorb.
            fx.script_status_failures(5, GRAPHQL_OUTAGE);
            let watched =
                with_completion_clock(|| action(&host, "pr_checks_watch", &watch_input(&fx, 600)))
                    .expect("a transient outage shorter than the watch is not a failure");
            assert_eq!(watched["recorded"], true, "{watched}");
            assert_eq!(
                fx.forge_state("status-faults").as_deref(),
                Some("0"),
                "every scripted failure went through the watcher's read path"
            );
            let observed = observations(&host);
            assert_eq!(observed.len(), 1, "{observed:?}");
            let note = observed[0].observation().expect("a decodable note");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Passed,
                "{note:?}"
            );
            assert_eq!(note.hold_reason(), None, "{note:?}");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert_no_mutation(&fx);
        },
    );
}

/// An outage that outlasts the watch records a held `pending` observation
/// naming nothing it did not see, and the run does not fail.
#[test]
fn a_transient_status_read_failure_through_the_deadline_holds_pending() {
    isolated(
        "review_watch::a_transient_status_read_failure_through_the_deadline_holds_pending",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            fx.script_status_failures(1_000_000, GRAPHQL_OUTAGE);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
            let now = std::rc::Rc::clone(&clock);
            let calls = std::cell::Cell::new(0_u32);
            let waited = std::rc::Rc::clone(&clock);
            let waits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let recorded_waits = std::rc::Rc::clone(&waits);
            let mut input = watch_input(&fx, 60);
            input["poll_interval_seconds"] = json!(600);
            let watched = orbit_engine::test_support::with_prerequisite_clock(
                move || {
                    let count = calls.get();
                    calls.set(count + 1);
                    if count == 1 {
                        now.set(now.get() + 50_000);
                    }
                    now.get()
                },
                |_, _| {},
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| {
                            let remaining = 1_060_000 - waited.get();
                            let millis = u64::try_from(duration.as_millis()).unwrap();
                            recorded_waits.borrow_mut().push((millis, remaining));
                            waited.set(waited.get() + millis);
                        },
                        || action(&host, "pr_checks_watch", &input),
                    )
                },
            )
            .expect("an outage through the deadline is a hold, not a failed run");
            let waits = waits.borrow();
            let &(last_wait, remaining) = waits.last().expect("the watch waited");
            assert!(
                remaining < 60_000,
                "the read consumed part of the budget: {waits:?}"
            );
            assert_eq!(
                last_wait, remaining,
                "the watch waits only to expiry: {waits:?}"
            );
            assert_eq!(clock.get(), 1_060_000, "the watch ends at expiry");
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed = observations(&host);
            assert_eq!(observed.len(), 1, "{observed:?}");
            let note = observed[0].observation().expect("a decodable note");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Pending,
                "{note:?}"
            );
            assert_eq!(note.head_sha(), None, "{note:?}");
            assert_eq!(note.failed_checks(), None, "{note:?}");
            assert!(note.hold_reason().is_some(), "{note:?}");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert_no_mutation(&fx);
        },
    );
}

/// A permanent read failure is not an outage to wait out.
#[test]
fn a_permanent_status_read_failure_still_fails_promptly() {
    isolated(
        "review_watch::a_permanent_status_read_failure_still_fails_promptly",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            fx.script_status_failures(1_000_000, "gh: Bad credentials (HTTP 401)");
            let error =
                with_completion_clock(|| action(&host, "pr_checks_watch", &watch_input(&fx, 600)))
                    .expect_err("a credential refusal fails the watch");
            assert!(error.to_string().contains("Bad credentials"), "{error}");
            assert!(observations(&host).is_empty());
            assert_no_mutation(&fx);
        },
    );
}

/// One push run on `sha` whose `test` job concluded `conclusion`, served to
/// the substitute `gh` as the commit's jobs and failed-job log.
fn plant_run(fx: &Fixture, sha: &str, run_id: u64, conclusion: &str, log: &str) {
    let dir = fx.forge.join("commit-ci").join(sha);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run_id"), run_id.to_string()).unwrap();
    let runs = json!({
        "total_count": 1,
        "workflow_runs": [{
            "id": run_id, "name": "CI", "workflow_id": 1, "event": "push",
            "head_sha": sha, "head_branch": BASE
        }]
    });
    fs::write(dir.join("runs.json"), serde_json::to_vec(&runs).unwrap()).unwrap();
    let jobs = json!({
        "total_count": 1,
        "jobs": [{
            "id": run_id + 1, "name": "test", "status": "completed", "conclusion": conclusion,
            "steps": [{"name": "compile", "conclusion": conclusion}]
        }]
    });
    fs::write(dir.join("jobs.json"), serde_json::to_vec(&jobs).unwrap()).unwrap();
    fs::write(dir.join("log"), log).unwrap();
}
