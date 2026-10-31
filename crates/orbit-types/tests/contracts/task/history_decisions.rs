//! The typed records automation reads from task history: each is a
//! [`TaskDecision`] on its event, persisted under the decision's `kind`, and
//! an entry written before typed decisions still yields it from its note
//! alone. A decision of another kind is never read through the note.

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, TaskDecision,
    TaskHistoryEntry,
};
use orbit_types::workflow::{
    BaselineRedFailures, CANDIDATE_HELD_EVENT, FLAKY_VALIDATION_EVENT, FlakyValidation,
    HeldCandidate, KNOWN_RED_VALIDATION_EVENT, KnownRedValidation,
};
use serde_json::{Value, json};

fn entry(event: &str, note: String, decision: Option<TaskDecision>) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: event.into(),
        note: Some(note),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision,
    }
}

fn held() -> HeldCandidate {
    HeldCandidate {
        run_id: "jrun-held".into(),
        machine_id: Some("machine-a".into()),
        branch: "orbit/ORB-00001-held".into(),
        head_sha: "a".repeat(40),
        failed_step_id: "validate".into(),
        durable_ref: Some("refs/orbit/candidates/ORB-00001/jrun-held".into()),
        carry_failure: None,
        task_spec_digest: "spec".into(),
    }
}

fn widening() -> ContextFilesWidening {
    ContextFilesWidening {
        run_id: "jrun-widen".into(),
        step: ContextWideningStep::Recovery,
        activity: "final_recovery".into(),
        selectors: vec!["file:src/widened.rs".into()],
    }
}

fn known_red() -> KnownRedValidation {
    KnownRedValidation {
        command: "make ci-test-affected".into(),
        base_sha: "b".repeat(40),
        tested_head: "c".repeat(40),
        run_id: "jrun-red".into(),
        failures: BaselineRedFailures {
            identities: vec!["test suite::broken".into()],
            ..Default::default()
        },
    }
}

fn flaky() -> FlakyValidation {
    let record = FlakyValidation {
        command: "make ci-test-affected".into(),
        base_sha: "b".repeat(40),
        tested_head: "c".repeat(40),
        run_id: "jrun-flaky".into(),
        failures: BTreeSet::from(["test suite::flaky".to_string()]),
        selection: json!({"failing_tests": ["suite::flaky"]}),
        original_exit_code: 100,
        original_output: "FAIL suite::flaky".into(),
        base_output: "ok".into(),
        rerun_output: "PASS suite::flaky".into(),
        rerun_tests_run: 1,
        validation_env: json!({}),
    };
    assert!(record.valid());
    record
}

/// One record's history shapes, read through its `from_entry`.
struct Shapes<T> {
    event: &'static str,
    kind: &'static str,
    record: T,
    note: String,
    decision: TaskDecision,
    read: fn(&TaskHistoryEntry) -> Option<T>,
}

impl<T: PartialEq + std::fmt::Debug> Shapes<T> {
    fn check(self) {
        let kind = self.kind;
        let typed = entry(self.event, self.note.clone(), Some(self.decision.clone()));
        assert_eq!((self.read)(&typed).as_ref(), Some(&self.record), "{kind}");

        // The persisted shape: the decision under its `kind`, beside the
        // unchanged note, read back whole.
        let stored = serde_json::to_value(&typed).unwrap();
        assert_eq!(stored["decision"]["kind"], kind, "{stored}");
        assert_eq!(stored["note"], Value::String(self.note.clone()), "{kind}");
        let restored: TaskHistoryEntry = serde_json::from_value(stored).unwrap();
        assert_eq!(restored, typed, "{kind}");

        let legacy = entry(self.event, self.note.clone(), None);
        assert_eq!(
            (self.read)(&legacy).as_ref(),
            Some(&self.record),
            "{kind}: an entry written before typed decisions is read from its note"
        );

        let other = entry(
            self.event,
            self.note,
            Some(TaskDecision::DrainApprovalHeld {
                classification: "other".into(),
            }),
        );
        assert_eq!(
            (self.read)(&other),
            None,
            "{kind}: a decision of another kind is not read through the note"
        );
    }
}

#[test]
fn each_history_record_is_read_from_its_decision_or_a_legacy_note() {
    let record = held();
    Shapes {
        event: CANDIDATE_HELD_EVENT,
        kind: "candidate_held",
        note: record.text(),
        decision: TaskDecision::CandidateHeld(record.clone()),
        record,
        read: HeldCandidate::from_entry,
    }
    .check();

    let record = widening();
    Shapes {
        event: CONTEXT_FILES_WIDENED_EVENT,
        kind: "context_files_widened",
        note: serde_json::to_string(&record).unwrap(),
        decision: TaskDecision::ContextFilesWidened(record.clone()),
        record,
        read: ContextFilesWidening::from_entry,
    }
    .check();

    let record = known_red();
    Shapes {
        event: KNOWN_RED_VALIDATION_EVENT,
        kind: "known_red_validation",
        note: record.text("Required validation fails only with tests the base fails too."),
        decision: TaskDecision::KnownRedValidation(record.clone()),
        record,
        read: KnownRedValidation::from_entry,
    }
    .check();

    let record = flaky();
    Shapes {
        event: FLAKY_VALIDATION_EVENT,
        kind: "flaky_validation",
        note: record.text(),
        decision: TaskDecision::FlakyValidation(record.clone()),
        record,
        read: FlakyValidation::from_entry,
    }
    .check();
}

/// A flaky record that fails its own bounds is none, whichever shape
/// carries it, as the note reader always refused it.
#[test]
fn an_invalid_flaky_record_is_refused_in_either_shape() {
    let mut record = flaky();
    record.rerun_tests_run = 0;
    let typed = entry(
        FLAKY_VALIDATION_EVENT,
        record.text(),
        Some(TaskDecision::FlakyValidation(record.clone())),
    );
    assert_eq!(FlakyValidation::from_entry(&typed), None);
    let legacy = entry(FLAKY_VALIDATION_EVENT, record.text(), None);
    assert_eq!(FlakyValidation::from_entry(&legacy), None);
}

/// Binding invalidation must not erase a recorded answer or change expiry
/// while the bound hold remains current.
#[test]
fn invalidated_decision_bindings_preserve_answers_and_current_expiry() {
    use orbit_types::task::{
        DECISION_ANSWERED_EVENT, DecisionBinding, DecisionGate, DecisionRequest,
        DecisionRequestState, DecisionResolution, decision_requests_from_history,
    };
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let task: orbit_types::task::Task = serde_json::from_value(json!({
        "id": "ORB-1", "title": "Held task", "description": "Fixture.",
        "context_files": [], "status": "backlog", "priority": "medium",
        "task_type": "chore", "created_at": now, "updated_at": now,
    }))
    .unwrap();
    let request = DecisionRequest::new(
        "ORB-1".into(),
        DecisionGate::PilotAdmission,
        vec![],
        DecisionBinding::PilotOperation {
            operation_id: "held-assessment".into(),
        },
        "task_pilot".into(),
        now,
    );
    let mut history = vec![request.history_entry("task-pilot")];
    assert_eq!(
        decision_requests_from_history(&task, &history, Some("held-assessment"), now)[0].state,
        DecisionRequestState::Open
    );
    assert_eq!(
        decision_requests_from_history(
            &task,
            &history,
            Some("held-assessment"),
            request.expires_at
        )[0]
        .state,
        DecisionRequestState::Expired
    );
    assert_eq!(
        decision_requests_from_history(&task, &history, None, now)[0].state,
        DecisionRequestState::Superseded
    );
    let resolution = DecisionResolution {
        request_id: request.id.clone(),
        answer_id: "clear".into(),
        answered_by: "human:fixture".into(),
        provenance: None,
        note: None,
        answered_at: now,
    };
    history.push(entry(
        DECISION_ANSWERED_EVENT,
        serde_json::to_string(&resolution).unwrap(),
        None,
    ));
    let answered = decision_requests_from_history(&task, &history, None, request.expires_at);
    assert_eq!(answered[0].state, DecisionRequestState::Answered);
    assert_eq!(answered[0].resolution.as_ref(), Some(&resolution));
}

/// A task-revision approval request stays superseded once an edit, status
/// move or candidate change follows its raise, even if the task is restored.
#[test]
fn a_task_revision_request_stays_superseded_after_the_task_is_restored() {
    use orbit_types::task::{
        DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState,
        TASK_FIELDS_UPDATED_EVENT, TaskStatus, decision_requests_from_history,
        task_fields_updated_note,
    };
    let raised_at = chrono::DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let task: orbit_types::task::Task = serde_json::from_value(json!({
        "id": "ORB-1", "title": "Proposal", "description": "Fixture.",
        "context_files": [], "status": "proposed", "priority": "medium",
        "task_type": "chore", "created_at": raised_at, "updated_at": raised_at,
    }))
    .unwrap();
    let request = DecisionRequest::new(
        "ORB-1".into(),
        DecisionGate::ProposedApproval,
        vec![],
        DecisionBinding::for_task(&task, None),
        "digest".into(),
        raised_at,
    );
    let state = |history: &[TaskHistoryEntry]| {
        decision_requests_from_history(&task, history, None, raised_at)[0].state
    };
    let later =
        |event: &str, note: Option<String>, from: Option<TaskStatus>, to: Option<TaskStatus>| {
            let mut entry = entry(event, String::new(), None);
            entry.at = raised_at + chrono::Duration::seconds(1);
            entry.note = note;
            entry.from_status = from;
            entry.to_status = to;
            entry
        };
    let raise = request.history_entry("digest");
    assert_eq!(
        state(std::slice::from_ref(&raise)),
        DecisionRequestState::Open
    );

    // Bookkeeping that does not change what the request rendered is harmless.
    let bookkeeping = later(
        TASK_FIELDS_UPDATED_EVENT,
        Some(task_fields_updated_note(&["plan"])),
        None,
        None,
    );
    assert_eq!(
        state(&[raise.clone(), bookkeeping]),
        DecisionRequestState::Open
    );

    // A spec edit supersedes it permanently although the task now reads as
    // it did when raised.
    let edit = later(
        TASK_FIELDS_UPDATED_EVENT,
        Some(task_fields_updated_note(&["description"])),
        None,
        None,
    );
    assert_eq!(
        state(&[raise.clone(), edit]),
        DecisionRequestState::Superseded
    );

    // So does leaving the status and coming back.
    let moves = [
        later(
            "status_changed",
            None,
            Some(TaskStatus::Proposed),
            Some(TaskStatus::Backlog),
        ),
        later(
            "status_changed",
            None,
            Some(TaskStatus::Backlog),
            Some(TaskStatus::Proposed),
        ),
    ];
    assert_eq!(
        state(&[raise, moves[0].clone(), moves[1].clone()]),
        DecisionRequestState::Superseded
    );
}
