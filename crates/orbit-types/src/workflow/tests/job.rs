//! [ORB-10002] Job run state machine tests for the `interrupted` state.

use std::str::FromStr;

use crate::workflow::{JobRunState, RunEvent};

#[test]
fn running_interrupt_transitions_to_interrupted() {
    assert_eq!(
        JobRunState::Running.try_transition(RunEvent::Interrupt),
        Ok(JobRunState::Interrupted)
    );
}

#[test]
fn interrupted_is_terminal_and_rejects_further_events() {
    assert!(JobRunState::Interrupted.is_terminal());
    for event in [
        RunEvent::Start,
        RunEvent::Complete,
        RunEvent::Fail,
        RunEvent::Timeout,
        RunEvent::Cancel,
        RunEvent::Abandon,
        RunEvent::Interrupt,
    ] {
        assert!(
            JobRunState::Interrupted.try_transition(event).is_err(),
            "interrupted must reject {event}"
        );
    }
}

#[test]
fn pending_interrupt_transitions_to_interrupted() {
    // [ORB-10070] Orphaned queued runs (dead or never-claimed worker) finalize
    // as interrupted, the same terminal state as orphaned running runs.
    assert_eq!(
        JobRunState::Pending.try_transition(RunEvent::Interrupt),
        Ok(JobRunState::Interrupted)
    );
}

#[test]
fn interrupted_display_and_parse_round_trip() {
    assert_eq!(JobRunState::Interrupted.to_string(), "interrupted");
    assert_eq!(
        JobRunState::from_str("interrupted"),
        Ok(JobRunState::Interrupted)
    );
}

#[test]
fn interrupted_is_a_valid_step_state() {
    assert!(JobRunState::Interrupted.validate_step_state().is_ok());
}

/// [ORB-10965] Only the caller that won the transition may execute the run;
/// every duplicate delivery, including one from the owner itself, must not.
#[test]
fn only_a_won_start_grants_execution_authority() {
    use crate::workflow::JobRunStartOutcome;

    assert!(JobRunStartOutcome::Started.owns_execution());
    assert!(!JobRunStartOutcome::AlreadyStarted.owns_execution());
    assert!(!JobRunStartOutcome::NotFound.owns_execution());
}

/// [ORB-10965] Owner identity — pid paired with its start-time token — is the
/// equivalence key for a duplicate Start. A reused pid is not the same owner.
#[test]
fn run_owner_equivalence_pairs_pid_with_its_start_time_token() {
    use chrono::Utc;

    use crate::workflow::JobRun;

    let now = Utc::now();
    let mut run = JobRun {
        executed_on: None,
        run_id: "jrun-owner".to_string(),
        job_id: "job-owner".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: Some(4242),
        pid_start_time: Some("v1:99".to_string()),
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };

    let legacy = serde_json::to_value(&run).expect("legacy shape");
    assert!(legacy.get("executed_on").is_none());
    let loaded: JobRun = serde_json::from_value(legacy).expect("legacy run");
    assert_eq!(loaded.executed_on, None);
    assert!(run.is_owned_by(4242, Some("v1:99")));
    assert!(
        !run.is_owned_by(4242, Some("v1:100")),
        "a reused pid with a different start time is a different process"
    );
    assert!(!run.is_owned_by(4243, Some("v1:99")));

    // A host that cannot produce a start-time token still compares by pid.
    run.pid_start_time = None;
    assert!(run.is_owned_by(4242, None));
    assert!(!run.is_owned_by(4242, Some("v1:99")));
}
