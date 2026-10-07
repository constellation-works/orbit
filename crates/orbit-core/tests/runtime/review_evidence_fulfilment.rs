//! A Linux owner fulfils a review held only for Linux CodeQL evidence: the
//! clock tick dispatches one run, which runs the named command at the held
//! commit and attaches its result and log, or refuses with a typed reason.
//!
//! The candidate carries a stub `scripts/codeql-rust-local.sh` that speaks
//! the real script's output contract and records each run's `HEAD` and
//! arguments in its captured output.

use std::os::unix::fs::PermissionsExt;

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_core::application::review::{EVIDENCE_FULFILMENT_AUDIT, REVIEW_EVIDENCE_FULFILMENT_JOB};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    JobRunState, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewEvidenceHold,
    ReviewEvidenceKind, ReviewExternalEvidence, ValidationOutcome,
};
use serde_json::{Value, json};

use super::review_continuation::run_review_pipeline;
use super::review_gate_audit::Fixture;

const CODEQL: &str = "scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls";
const EVIDENCE: &str = "evidence/codeql-rust-linux.json";
const EVIDENCE_LOG: &str = "evidence/codeql-rust-linux.log.json";
const SHIPPED_JOB: &str =
    include_str!("../../assets/jobs/review_evidence_fulfilment_pipeline.yaml");
const SHIPPED_ACTIVITY: &str = include_str!("../../assets/activities/fulfil_review_evidence.yaml");
/// Far more free space than any test host has.
const UNREACHABLE_MIB: u64 = 1 << 40;

/// How the stub behaves once it has recorded its run.
#[derive(Clone, Copy)]
enum Stub {
    /// Complete analysis, no results.
    Clean,
    /// The real script's refusal of an extraction that skipped analysis.
    Incomplete,
    /// Complete analysis with one result.
    Findings,
}

fn stub_script(stub: Stub) -> String {
    let outcome = match stub {
        Stub::Clean => {
            r#"printf '{"runs":[{"tool":{"driver":{"name":"CodeQL"}},"results":[]}]}' >"$run_dir/results.sarif"
echo "codeql-rust-local: analysis completed; inspect rule and affected locations in $run_dir/results.sarif""#
        }
        Stub::Incomplete => {
            r#"echo "codeql-rust-local: incomplete Rust extraction (prepared Rust 1.97.0 with rust-src); semantic analysis must not be skipped" >&2
exit 1"#
        }
        Stub::Findings => {
            r#"printf '{"runs":[{"results":[{"ruleId":"rust/cleartext-logging"}]}]}' >"$run_dir/results.sarif"
echo "codeql-rust-local: analysis completed; inspect rule and affected locations in $run_dir/results.sarif""#
        }
    };
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
head="$(git rev-parse HEAD)"
echo "ORBIT_CODEQL_STUB_RUN: $head $*"
run_dir="$(mktemp -d "${{ORBIT_SCRATCH_DIR:?}}/codeql-rust-local.XXXXXX")"
echo "codeql-rust-local: run directory: $run_dir" >&2
{outcome}
"#
    )
}

fn git(fixture: &Fixture, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(args)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A task whose before-PR review settled into an evidence hold naming one
/// `codeql` run of `command`, on a candidate that carries the stub. The
/// workspace branch then moves past the held commit, so only a checkout of
/// the held commit runs the held stub.
fn held(stub: Stub, command: &str) -> (Fixture, ReviewEvidenceHold) {
    let mut fixture = Fixture::new_with_required_commands(&[command]);
    // A dispatched run's substitute worker stays alive until the test has run
    // the step in process, so the supervisor never interrupts it as pending.
    orbit_core::test_support::install_substitute_pipeline_worker([
        "sh".to_string(),
        "-c".to_string(),
        "i=0; while [ ! -e \"$1/ran-$2\" ] && [ $i -lt 1200 ]; do sleep 0.1; i=$((i+1)); done"
            .to_string(),
        "worker".to_string(),
        fixture._root.path().to_string_lossy().into_owned(),
        orbit_core::test_support::RUN_ID_PLACEHOLDER.to_string(),
    ]);
    let script = fixture.repo.join("scripts/codeql-rust-local.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, stub_script(stub)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&fixture, &["add", "scripts"]);
    git(&fixture, &["commit", "-q", "-m", "stub codeql"]);
    fixture.admit();
    fixture.put_report(&json!({
        "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": "Reviewed on macOS; the Linux CodeQL run is owed.",
        "findings": [],
        "validation": [
            {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
            {"id": "V2", "command": command, "outcome": "not_run", "role": "required"},
        ],
        "external_evidence": [{
            "kind": "codeql", "name": "Rust CodeQL (Linux)", "command": command, "artifact": EVIDENCE,
        }],
        "escalation": "A Linux CodeQL run is owed",
    }));
    run_review_pipeline(&fixture);
    let hold: ReviewEvidenceHold = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .expect("the review settled into an evidence hold")
            .content,
    )
    .unwrap();
    std::fs::write(fixture.repo.join("candidate.txt"), "later\n").unwrap();
    git(&fixture, &["commit", "-q", "-am", "moved on"]);
    assert_ne!(git(&fixture, &["rev-parse", "HEAD"]), hold.candidate.commit);
    (fixture, hold)
}

/// The shipped job, with the free space its tick and step require replaced.
fn configure_job(fixture: &Fixture, tick_mib: u64, step_mib: Option<u64>) {
    let mut job: serde_yaml::Value = serde_yaml::from_str(SHIPPED_JOB).unwrap();
    job["spec"]["default_input"]["min_free_mib"] = serde_yaml::Value::Number(tick_mib.into());
    if let Some(step_mib) = step_mib {
        let steps = job["spec"]["steps"]
            .as_sequence_mut()
            .expect("the shipped workflow has steps");
        let fulfil = steps
            .iter_mut()
            .find(|step| step["id"].as_str() == Some("fulfil"))
            .expect("the shipped workflow has a fulfil step");
        fulfil["default_input"]["min_free_mib"] = serde_yaml::Value::Number(step_mib.into());
    }
    // Shipped names resolve from the fixture's global catalog.
    let resources = fixture.runtime.paths().global_dir.join("resources");
    let activities = resources.join("activities");
    std::fs::create_dir_all(&activities).unwrap();
    std::fs::write(
        activities.join("fulfil_review_evidence.yaml"),
        SHIPPED_ACTIVITY,
    )
    .unwrap();
    let jobs = &resources.join("jobs");
    std::fs::create_dir_all(jobs).unwrap();
    std::fs::write(
        jobs.join(format!("{REVIEW_EVIDENCE_FULFILMENT_JOB}.yaml")),
        serde_yaml::to_string(&job).unwrap(),
    )
    .unwrap();
}

/// Run a dispatched run's worker in process, then release its substitute.
fn execute(fixture: &Fixture, run_id: &str) {
    let executed = fixture.runtime.execute_pipeline_run_worker(run_id);
    std::fs::write(fixture._root.path().join(format!("ran-{run_id}")), "").unwrap();
    executed.unwrap();
}

/// Dispatch through the tick, then run the dispatched worker in process.
fn fulfil_once(fixture: &Fixture) -> (String, Value) {
    let tick = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert_eq!(tick.dispatched.len(), 1, "{tick:?}");
    let (task_id, run_id) = tick.dispatched[0].clone();
    assert_eq!(task_id, fixture.task_id);
    execute(fixture, &run_id);
    let state = fixture.runtime.read_run_state(&run_id).unwrap().unwrap();
    (run_id, state.pipeline["fulfil"].clone())
}

fn stub_runs(fixture: &Fixture) -> Vec<String> {
    artifact(fixture, EVIDENCE_LOG)
        .and_then(|log| log["stdout"].as_str().map(str::to_string))
        .into_iter()
        .flat_map(|stdout| {
            stdout
                .lines()
                .filter_map(|line| line.strip_prefix("ORBIT_CODEQL_STUB_RUN: "))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn artifact(fixture: &Fixture, path: &str) -> Option<Value> {
    fixture
        .runtime
        .get_task_artifact(&fixture.task_id, path)
        .unwrap()
        .map(|artifact| serde_json::from_slice(&artifact.content).unwrap())
}

fn audit_rows(fixture: &Fixture) -> Vec<(AuditEventStatus, Value)> {
    fixture
        .runtime
        .list_audit_events(
            None,
            Some(EVIDENCE_FULFILMENT_AUDIT.into()),
            None,
            None,
            100,
        )
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.status,
                serde_json::from_str(row.arguments_json.as_deref().unwrap()).unwrap(),
            )
        })
        .collect()
}

/// The event of the task's latest delivery decision: a status change, or a
/// review decision that keeps the status.
fn latest_decision(fixture: &Fixture) -> String {
    fixture
        .runtime
        .get_task_history(&fixture.task_id)
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry.to_status.is_some() || entry.event.starts_with("review_"))
        .unwrap()
        .event
        .clone()
}

fn assert_still_held(fixture: &Fixture, why: &str) {
    let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
    assert_eq!(task.status, TaskStatus::InProgress, "{why}");
    assert_eq!(
        latest_decision(fixture),
        "review_awaiting_evidence",
        "{why}"
    );
    assert!(
        artifact(fixture, EVIDENCE).is_none(),
        "{why}: nothing is accepted"
    );
}

#[test]
fn a_held_codeql_check_runs_at_the_held_commit_and_its_result_requeues_review() {
    if !super::dispatch_admission::isolated(
        "review_evidence_fulfilment::a_held_codeql_check_runs_at_the_held_commit_and_its_result_requeues_review",
    ) {
        return;
    }
    let (fixture, mut hold) = held(Stub::Clean, CODEQL);
    configure_job(&fixture, 1, None);
    // A benign legacy spelling must write the same canonical evidence/log
    // pair as a new hold; suffix derivation must follow normalization.
    hold.requirements[0].artifact = " evidence//./codeql-rust-linux.json/ ".into();
    super::review_continuation::attach_as_operator(
        &fixture,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
        &serde_json::to_value(&hold).unwrap(),
    );

    if !orbit_exec::probe_bwrap().available {
        let deferred = fixture
            .runtime
            .run_review_evidence_fulfilment_tick(Utc::now())
            .unwrap();
        assert!(deferred.dispatched.is_empty(), "{deferred:?}");
        assert!(
            deferred
                .skipped
                .as_deref()
                .is_some_and(|reason| reason.contains("Bubblewrap")),
            "the owner must defer until Bubblewrap namespaces work: {deferred:?}"
        );
        assert_still_held(&fixture, "sandbox unavailable");
        return;
    }

    let tick = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert_eq!(tick.dispatched.len(), 1, "{tick:?}");
    let run_id = tick.dispatched[0].1.clone();
    let busy = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert!(
        busy.dispatched.is_empty(),
        "one fulfilment runs at a time: {busy:?}"
    );
    execute(&fixture, &run_id);

    assert_eq!(
        fixture.runtime.show_job_run(&run_id).unwrap().state,
        JobRunState::Success
    );
    let output = fixture
        .runtime
        .read_run_state(&run_id)
        .unwrap()
        .unwrap()
        .pipeline["fulfil"]
        .clone();
    assert_eq!(output["fulfilled"], true, "{output}");
    assert_eq!(output["requeued"], true, "{output}");
    assert_eq!(
        stub_runs(&fixture),
        vec![format!(
            "{} --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls",
            hold.candidate.commit
        )],
        "the named command ran once, at the held commit, without a shell"
    );

    let evidence: ReviewExternalEvidence =
        serde_json::from_value(artifact(&fixture, EVIDENCE).expect("result attached")).unwrap();
    assert_eq!(evidence.attempt_id, hold.attempt_id);
    assert_eq!(evidence.candidate, hold.candidate);
    assert_eq!(evidence.kind, ReviewEvidenceKind::CodeQl);
    assert_eq!(evidence.command, CODEQL);
    assert_eq!(evidence.outcome, ValidationOutcome::Passed);
    assert_eq!(evidence.log_artifact, EVIDENCE_LOG);
    let log = artifact(&fixture, EVIDENCE_LOG).expect("log attached");
    assert_eq!(log["tested_head"], hold.candidate.commit.as_str());
    assert_eq!(log["exit_code"], 0);
    assert_eq!(log["sarif"]["results"], 0);

    let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Backlog,
        "receipt queues a fresh review"
    );
    assert_eq!(latest_decision(&fixture), "review_evidence_received");
    let rows = audit_rows(&fixture);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, AuditEventStatus::Success);
    assert_eq!(rows[0].1["outcome"], "fulfilled");
    assert_eq!(rows[0].1["candidate"], hold.candidate.commit.as_str());
    assert!(
        !fixture
            .runtime
            .paths()
            .state_dir
            .join("recovery-checkouts")
            .join(format!("{run_id}-evidence"))
            .exists(),
        "the scratch checkout is removed"
    );

    let after = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert!(after.dispatched.is_empty(), "{after:?}");
}

#[test]
fn an_incomplete_failed_or_unadmitted_run_leaves_the_hold_with_a_typed_reason() {
    if !super::dispatch_admission::isolated(
        "review_evidence_fulfilment::an_incomplete_failed_or_unadmitted_run_leaves_the_hold_with_a_typed_reason",
    ) {
        return;
    }
    if !orbit_exec::probe_bwrap().available {
        return;
    }
    let unadmitted = "scripts/codeql-rust-local.sh codeql/rust-queries:x;touch owned";
    for (stub, command, reason, ran) in [
        (Stub::Incomplete, CODEQL, "analysis_incomplete", true),
        (Stub::Findings, CODEQL, "findings_reported", true),
        (Stub::Clean, unadmitted, "command_not_allowed", false),
    ] {
        let (fixture, hold) = held(stub, command);
        configure_job(&fixture, 1, None);
        let (_, output) = fulfil_once(&fixture);
        assert_eq!(output["fulfilled"], false, "{reason}: {output}");
        assert_eq!(output["reason"], reason, "{output}");
        assert_eq!(output["retryable"], false, "{output}");
        assert_eq!(stub_runs(&fixture).len(), usize::from(ran), "{reason}");
        assert!(!fixture.repo.join("owned").exists());
        assert_still_held(&fixture, reason);
        let log = artifact(&fixture, EVIDENCE_LOG).expect("the refused run's log is attached");
        assert_eq!(log["reason"], reason);
        assert_eq!(log["tested_head"], hold.candidate.commit.as_str());
        let rows = audit_rows(&fixture);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, AuditEventStatus::Failure, "{reason}");
        assert_eq!(rows[0].1["reason"], reason);
        let again = fixture
            .runtime
            .run_review_evidence_fulfilment_tick(Utc::now())
            .unwrap();
        assert!(
            again.dispatched.is_empty(),
            "{reason} needs a new decision, not another run: {again:?}"
        );
    }
}

#[test]
fn owner_fulfilment_refuses_reserved_aliases_in_a_persisted_hold_without_overwriting_review() {
    if !super::dispatch_admission::isolated(
        "review_evidence_fulfilment::owner_fulfilment_refuses_reserved_aliases_in_a_persisted_hold_without_overwriting_review",
    ) {
        return;
    }
    for reserved in [REVIEW_GATE_ARTIFACT, REVIEW_EVIDENCE_HOLD_ARTIFACT] {
        for alias in [
            format!(" {reserved}"),
            format!("{reserved} "),
            format!("{reserved}/"),
            format!("./{reserved}"),
        ] {
            let (fixture, mut hold) = held(Stub::Clean, CODEQL);
            configure_job(&fixture, 1, None);
            // Seed a hostile persisted hold after normal admission. This
            // reaches the owner's guard independently of the report guard.
            hold.requirements[0].artifact = alias.clone();
            super::review_continuation::attach_as_operator(
                &fixture,
                REVIEW_EVIDENCE_HOLD_ARTIFACT,
                &serde_json::to_value(&hold).unwrap(),
            );
            let before_gate = fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
                .unwrap()
                .unwrap()
                .content;
            let before_hold = fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .unwrap()
                .content;
            // Submit the actual owner workflow directly: the unsafe path
            // must be refused before any sandbox or CodeQL prerequisite.
            // Unlike a tick, this boundary check also runs on Linux hosts
            // where user namespaces are unavailable.
            let run = fixture
                .runtime
                .submit_pipeline_run(
                    REVIEW_EVIDENCE_FULFILMENT_JOB,
                    json!({
                        "task_id": fixture.task_id,
                        "hold_key": format!("{}:{}", hold.attempt_id, hold.candidate.commit),
                    }),
                    None,
                    Some("system"),
                )
                .unwrap();
            execute(&fixture, &run.run_id);
            let output = fixture
                .runtime
                .read_run_state(&run.run_id)
                .unwrap()
                .unwrap()
                .pipeline["fulfil"]
                .clone();
            assert_eq!(output["fulfilled"], false, "{alias}: {output}");
            assert_eq!(
                output["reason"], "artifact_not_allowed",
                "{alias}: {output}"
            );
            assert_eq!(
                fixture
                    .runtime
                    .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
                    .unwrap()
                    .unwrap()
                    .content,
                before_gate,
                "ORB-14472: owner evidence must never overwrite the review certificate via {alias}",
            );
            assert_eq!(
                fixture
                    .runtime
                    .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                    .unwrap()
                    .unwrap()
                    .content,
                before_hold,
                "ORB-14472: owner evidence must never overwrite the hold via {alias}",
            );
            assert!(artifact(&fixture, EVIDENCE).is_none());
            assert!(
                artifact(&fixture, EVIDENCE_LOG).is_none(),
                "unsafe requirement attaches no log"
            );
            assert_still_held(&fixture, &alias);
        }
    }
}

#[test]
fn fulfilment_waits_for_disk_and_retries_a_disk_refusal_a_bounded_number_of_times() {
    if !super::dispatch_admission::isolated(
        "review_evidence_fulfilment::fulfilment_waits_for_disk_and_retries_a_disk_refusal_a_bounded_number_of_times",
    ) {
        return;
    }
    if !orbit_exec::probe_bwrap().available {
        return;
    }
    let (fixture, _) = held(Stub::Clean, CODEQL);

    configure_job(&fixture, UNREACHABLE_MIB, None);
    let deferred = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert!(deferred.dispatched.is_empty(), "{deferred:?}");
    assert!(deferred.skipped.is_some(), "{deferred:?}");

    // The tick sees room, but the run's own gate does not.
    configure_job(&fixture, 1, Some(UNREACHABLE_MIB));
    for attempt in 1..=3 {
        let (_, output) = fulfil_once(&fixture);
        assert_eq!(
            output["reason"], "disk_insufficient",
            "attempt {attempt}: {output}"
        );
        assert_eq!(output["retryable"], true, "{output}");
    }
    let exhausted = fixture
        .runtime
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    assert!(exhausted.dispatched.is_empty(), "{exhausted:?}");
    assert!(
        stub_runs(&fixture).is_empty(),
        "no run starts short of disk"
    );
    assert_still_held(&fixture, "disk_insufficient");
    let rows = audit_rows(&fixture);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        rows.iter()
            .all(|(status, detail)| *status == AuditEventStatus::Failure
                && detail["reason"] == "disk_insufficient"
                && detail["min_free_mib"] == UNREACHABLE_MIB),
        "{rows:?}"
    );
}
