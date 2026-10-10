//! Automation checkpoints through public composition, a separate store area
//! from task admission and readiness. Mutable fixtures run in isolated children.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::process::Command;

use chrono::{DateTime, Utc};
use orbit_common::{process, test_env};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::recovery::{
    AutomationStall, CoverageDebt, RecoveryRecord, ResetRecord,
};
use orbit_types::workflow::automation::{
    AutomationState, BatchAttempt, BatchState, BatchWaiver, CoverageBatch, CoverageClass, Delivery,
    DeliveryTrigger, SourceRevision,
};

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_AUTOMATION_STORE_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return false;
    }
    let home = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output =
        process::run_bounded_capped(&mut command, test_env::CHILD_TEST_DEADLINE, 64 * 1024)
            .expect("run isolated automation fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    true
}

fn now() -> DateTime<Utc> {
    "2026-01-01T00:00:00Z".parse().unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Write {
    Commit,
    Recover,
    Stall,
    Reset,
    Waive,
}

fn baseline() -> AutomationState {
    let revision = SourceRevision {
        commit: "base".into(),
        tree: "base-tree".into(),
    };
    AutomationState {
        members: None,
        consumer: "fixture/legacy".into(),
        epoch: "epoch".into(),
        trigger: Some(DeliveryTrigger {
            owner_machine: None,
            branch: "fixture-branch".into(),
            threshold: 1,
            max_wait_minutes: 10,
            coverage: CoverageClass::LandedCodeReviewV1,
            max_items: 1,
            retries: 0,
        }),
        repository: "fixture-repo".into(),
        branch: "fixture-branch".into(),
        generation: 0,
        baseline: revision.clone(),
        observed: revision.clone(),
        covered: revision,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        active: None,
        stall: None,
    }
}

fn failed_batch(state: &mut AutomationState) {
    let after = SourceRevision {
        commit: "landing".into(),
        tree: "landing-tree".into(),
    };
    let delivery = Delivery {
        key: "delivery".into(),
        repository: state.repository.clone(),
        branch: state.branch.clone(),
        before: state.baseline.clone(),
        after: after.clone(),
        commits: vec![after.commit.clone()],
        task_ids: vec![],
        unattributed: None,
        evidence_reference: "fixture-evidence".into(),
        evidence_digest: "fixture-digest".into(),
        landed_at: now(),
    };
    state.pending_commits = delivery.commits.clone();
    state.pending = vec![delivery.clone()];
    state.observed = after.clone();
    state.active = Some(BatchAttempt {
        batch: CoverageBatch {
            schema_version: 1,
            id: "batch".into(),
            consumer: state.consumer.clone(),
            epoch: state.epoch.clone(),
            repository: state.repository.clone(),
            branch: state.branch.clone(),
            coverage: CoverageClass::LandedCodeReviewV1,
            from_exclusive: state.baseline.clone(),
            through_inclusive: after,
            commits: delivery.commits.clone(),
            deliveries: vec![delivery],
            exclusions: vec![],
            created_at: now(),
            max_attempts: 1,
            retry_until: now(),
        },
        input_digest: "input-digest".into(),
        attempt: 1,
        action_key: "action-key".into(),
        action_id: Some("action".into()),
        state: BatchState::Failed,
        reason: Some("fixture-failure".into()),
        retry_after: None,
        reissue: None,
    });
}

fn next_state(previous: &AutomationState, write: Write) -> AutomationState {
    let mut next = previous.clone();
    next.generation += 1;
    match write {
        Write::Recover => next.epoch = "adopted-epoch".into(),
        Write::Stall => {
            next.stall = Some(AutomationStall {
                reason: "fixture-stall".into(),
                since: now(),
                escalated_at: None,
                friction_id: None,
                divergence: None,
            });
        }
        Write::Waive => {
            next.active = None;
            next.waived.append(&mut next.pending);
        }
        Write::Commit | Write::Reset => {}
    }
    next
}

fn recovery(previous: &AutomationState, next: &AutomationState, write: Write) -> RecoveryRecord {
    RecoveryRecord {
        consumer: previous.consumer.clone(),
        previous_epoch: previous.epoch.clone(),
        epoch: next.epoch.clone(),
        previous_trigger: previous.trigger.clone(),
        trigger: next.trigger.clone(),
        adopted_settings: matches!(write, Write::Recover),
        reissued: None,
        replayed_history: None,
        reset: matches!(write, Write::Reset).then(|| ResetRecord {
            previous_generation: previous.generation,
            forgotten: CoverageDebt {
                baseline: previous.baseline.clone(),
                covered: previous.covered.clone(),
                observed: previous.observed.clone(),
                pending_deliveries: previous.pending.len(),
                pending_commits: previous.pending_commits.len(),
                unresolved: previous.unresolved.len(),
                waived: previous.waived.len(),
                excluded: previous.excluded.len(),
                receipts: 0,
            },
            abandoned_action: None,
            baseline: previous.baseline.clone(),
            released_refs: vec![],
            cleared_stall: previous.stall.clone(),
        }),
        friction_id: None,
        reason: "authorized fixture recovery".into(),
        by: "fixture-operator".into(),
        at: now(),
    }
}

fn apply(store: &dyn AutomationStoreBackend, previous: &AutomationState, write: Write) -> bool {
    let next = next_state(previous, write);
    match write {
        Write::Commit => store.automation_commit(previous, &next, None),
        Write::Recover => {
            store.automation_recover(previous, &next, &recovery(previous, &next, write))
        }
        Write::Stall => store.automation_stall(previous, &next),
        Write::Reset => store.automation_reset(previous, &recovery(previous, previous, write)),
        Write::Waive => store.automation_waive(
            previous,
            &next,
            &BatchWaiver {
                batch_id: previous.active.as_ref().unwrap().batch.id.clone(),
                reason: "authorized fixture waiver".into(),
                by: "fixture-operator".into(),
                at: now(),
            },
        ),
    }
    .unwrap()
}

#[test]
fn consumer_writes_accept_legacy_json_and_refuse_stale_snapshots() {
    if isolated("consumer_writes_accept_legacy_json_and_refuse_stale_snapshots") {
        return;
    }
    for missing in [
        "excluded",
        "waived",
        "associations",
        "lookup_retries",
        "trigger.retries",
        "formatting",
        "field_order",
    ] {
        for write in [
            Write::Commit,
            Write::Recover,
            Write::Stall,
            Write::Reset,
            Write::Waive,
        ] {
            let base = Store::open_in_memory().unwrap();
            let store = compose::automation_store(base.clone()).unwrap();
            let mut expected = baseline();
            if matches!(write, Write::Waive) {
                failed_batch(&mut expected);
            }
            let mut json = serde_json::to_value(&expected).unwrap();
            match missing {
                "trigger.retries" => {
                    json["trigger"].as_object_mut().unwrap().remove("retries");
                }
                "formatting" | "field_order" => {}
                key => {
                    json.as_object_mut().unwrap().remove(key);
                }
            }
            // Reverse the stored key order explicitly: the workspace preserves
            // insertion order in JSON objects. The order-only control uses
            // compact JSON; the other cases also exercise pretty printing.
            let object = json.as_object_mut().unwrap();
            *object = std::mem::take(object).into_iter().rev().collect();
            let raw = if missing == "field_order" {
                serde_json::to_string(&json).unwrap()
            } else {
                serde_json::to_string_pretty(&json).unwrap()
            };
            base.connection()
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO automation_consumers VALUES (?1,?2,?3)",
                    rusqlite::params![expected.consumer, expected.generation, raw],
                )
                .unwrap();
            let previous = store.automation_state(&expected.consumer).unwrap().unwrap();
            assert_eq!(previous, expected, "{missing}: legacy snapshot decodes");

            let mut altered = previous.clone();
            altered.repository = "other-repository".into();
            assert!(
                !apply(store.as_ref(), &altered, write),
                "{missing}/{write:?}: same generation does not authorize an altered snapshot"
            );
            assert_eq!(
                store.automation_state(&previous.consumer).unwrap(),
                Some(previous.clone())
            );
            assert!(
                store
                    .automation_recoveries(&previous.consumer, 10)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                store
                    .automation_waivers(&previous.consumer, 10)
                    .unwrap()
                    .is_empty()
            );

            assert!(
                apply(store.as_ref(), &previous, write),
                "{missing}/{write:?}: legacy checkpoint must advance"
            );
            let persisted = store.automation_state(&previous.consumer).unwrap();
            if matches!(write, Write::Reset) {
                assert!(persisted.is_none());
            } else {
                assert_eq!(persisted, Some(next_state(&previous, write)));
            }
            assert!(
                !apply(store.as_ref(), &previous, write),
                "{missing}/{write:?}: stale writer must change nothing"
            );
            assert_eq!(
                store.automation_state(&previous.consumer).unwrap(),
                persisted
            );
            let records = store.automation_recoveries(&previous.consumer, 10).unwrap();
            let waivers = store.automation_waivers(&previous.consumer, 10).unwrap();
            match write {
                Write::Recover => assert_eq!(
                    records,
                    vec![recovery(&previous, &next_state(&previous, write), write)]
                ),
                Write::Reset => {
                    assert_eq!(records, vec![recovery(&previous, &previous, write)]);
                    let mut replacement = baseline();
                    replacement.epoch = "replacement-epoch".into();
                    assert!(store.automation_initialize(&replacement).unwrap());
                    assert!(
                        !apply(store.as_ref(), &previous, write),
                        "a reset must not delete a new incarnation at the same generation"
                    );
                    assert_eq!(
                        store.automation_state(&previous.consumer).unwrap(),
                        Some(replacement)
                    );
                    assert_eq!(
                        store.automation_recoveries(&previous.consumer, 10).unwrap(),
                        records
                    );
                }
                Write::Waive => {
                    assert_eq!(waivers.len(), 1);
                    assert_eq!(
                        waivers[0].batch_id,
                        previous.active.as_ref().unwrap().batch.id
                    );
                }
                Write::Commit | Write::Stall => assert!(records.is_empty() && waivers.is_empty()),
            }
        }
    }
}

#[test]
fn concurrent_action_key_admission_initializes_one_run_and_resolves_without_writes() {
    if isolated("concurrent_action_key_admission_initializes_one_run_and_resolves_without_writes") {
        return;
    }
    use orbit_store::contracts::KeyedJobRunAdmission;
    use serde_json::json;
    use std::sync::{Arc, Barrier};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let base = Store::open(&path).unwrap();
    let jobs = compose::workspace_job_run_store(base.clone(), "workspace");
    let other = compose::workspace_job_run_store(Store::open(&path).unwrap(), "workspace");
    let barrier = Arc::new(Barrier::new(2));
    let input = json!({"batch": "fixture"});
    let admissions = [jobs.clone(), other].map(|store| {
        let barrier = barrier.clone();
        let input = input.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store
                .insert_automation_job_run("automation_fixture", input, "action")
                .unwrap()
        })
    });
    let outcomes = admissions.map(|thread| thread.join().unwrap());
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, KeyedJobRunAdmission::Admitted(_)))
            .count(),
        1,
        "concurrent submissions must identify exactly one initializer"
    );
    let run_ids = outcomes.map(|outcome| match outcome {
        KeyedJobRunAdmission::Admitted(run) | KeyedJobRunAdmission::Existing(run) => run.run_id,
    });
    assert_eq!(run_ids[0], run_ids[1]);
    assert_eq!(jobs.list_job_runs("automation_fixture").unwrap().len(), 1);
    let run_id = &run_ids[0];
    let mut state = jobs.read_run_state(run_id).unwrap().unwrap();
    state.next_step_index = 3;
    state.step_outputs.insert(2, json!({"checkpoint": true}));
    jobs.write_run_state(run_id, &state).unwrap();
    base.connection()
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_run_rewrite BEFORE UPDATE ON job_runs \
         BEGIN SELECT RAISE(ABORT, 'existing admission rewrote a run'); END;",
        )
        .unwrap();

    let existing = jobs
        .insert_automation_job_run("automation_fixture", input.clone(), "action")
        .unwrap();
    assert!(matches!(existing, KeyedJobRunAdmission::Existing(run) if run.run_id == *run_id));
    for (job, altered) in [
        ("automation_fixture", json!({"batch": "changed"})),
        ("other_job", input),
    ] {
        assert!(matches!(
            jobs.insert_automation_job_run(job, altered, "action"),
            Err(orbit_common::OrbitError::InvalidInput(_))
        ));
    }
    assert_eq!(jobs.read_run_state(run_id).unwrap(), Some(state));
}

/// The automation v1-v3 tables and ledger rows, as a binary before the
/// one-time member repairs left a store.
fn automation_schema_v3(base: &Store) {
    base.connection()
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TABLE automation_consumers (consumer TEXT PRIMARY KEY, generation INTEGER NOT NULL, state_json TEXT NOT NULL);
             CREATE TABLE automation_coverage (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, receipt_json TEXT NOT NULL, accepted_at TEXT NOT NULL);
             CREATE INDEX automation_coverage_consumer ON automation_coverage(consumer, accepted_at);
             CREATE TABLE automation_delivery_intents (record_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT NOT NULL, delivery_json TEXT NOT NULL);
             CREATE TABLE automation_delivery_members (repository TEXT NOT NULL, branch TEXT NOT NULL, commit_id TEXT NOT NULL, record_id TEXT NOT NULL, PRIMARY KEY(repository,branch,commit_id,record_id));
             CREATE TABLE automation_waivers (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, waiver_json TEXT NOT NULL);
             CREATE TABLE automation_job_keys (workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, run_id TEXT NOT NULL, PRIMARY KEY(workspace_id,action_key));
             CREATE INDEX IF NOT EXISTS job_runs_retry_lineage ON job_runs(workspace_id,retry_source_run_id);
             CREATE TABLE automation_recoveries (record_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, recorded_at TEXT NOT NULL, record_json TEXT NOT NULL);
             CREATE INDEX automation_recoveries_consumer ON automation_recoveries(consumer, recorded_at);
             INSERT INTO feature_schema_meta(feature, version, name, applied_at) VALUES
               ('automation', 1, 'consumer_checkpoints_and_coverage', '2026-01-01T00:00:00Z'),
               ('automation', 2, 'retry_lineage_index', '2026-01-01T00:00:00Z'),
               ('automation', 3, 'consumer_recovery_records', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
}

/// A task-pilot member a branch move shelved before supersession existed: its
/// exhausted failure record sits at the fingerprint it is still pending at,
/// while it is pending at a newer source. The one-time repair releases it so
/// the next pass admits it again; a record at the pending source stays.
#[test]
fn feature_repair_releases_members_shelved_at_a_source_the_branch_left() {
    if isolated("feature_repair_releases_members_shelved_at_a_source_the_branch_left") {
        return;
    }
    use orbit_types::workflow::automation::members::{
        MemberAttempt, MemberState, StateMember, StateTriggerKind,
    };
    let revision = |commit: &str| SourceRevision {
        commit: commit.into(),
        tree: format!("{commit}-tree"),
    };
    let member = |key: &str, source: &str| StateMember {
        key: key.into(),
        task_ids: vec![key.into()],
        fingerprint: format!("{key}-fingerprint"),
        source: revision(source),
        evidence: serde_json::json!({"task_id": key}),
        first_seen: now(),
        changed_at: now(),
        crew: None,
    };
    let shelved = |member: StateMember| MemberAttempt {
        consumer: "fixture/pilot".into(),
        kind: StateTriggerKind::PreparationEligible,
        id: format!("attempt-{}", member.key),
        member,
        members: vec![],
        attempt: 2,
        max_attempts: 2,
        deadline: now(),
        retry_after: now(),
        action_key: "automation:attempt:2".into(),
        action_id: Some("run".into()),
        exhausted: true,
    };
    let mut state = baseline();
    state.consumer = "fixture/pilot".into();
    state.trigger = None;
    state.generation = 7;
    state.members = Some(MemberState {
        pending: [
            ("moved".into(), member("moved", "head")),
            ("unmoved".into(), member("unmoved", "frozen")),
        ]
        .into(),
        failed: [
            ("moved".into(), shelved(member("moved", "frozen"))),
            ("unmoved".into(), shelved(member("unmoved", "frozen"))),
        ]
        .into(),
        withheld: [
            ("moved".into(), "stopped_without_member_evidence".into()),
            ("unmoved".into(), "stopped_without_member_evidence".into()),
        ]
        .into(),
        ..Default::default()
    });

    // A store at automation schema v3, as the binary before the repair left
    // it: the v1-v3 tables and their ledger rows, then the shelved state.
    let base = Store::open_in_memory().unwrap();
    automation_schema_v3(&base);
    {
        let connection = base.connection();
        let connection = connection.lock().unwrap();
        connection
            .execute(
                "INSERT INTO automation_consumers VALUES (?1,?2,?3)",
                rusqlite::params![
                    state.consumer,
                    state.generation,
                    serde_json::to_string(&state).unwrap()
                ],
            )
            .unwrap();
    }
    let store = compose::automation_store(base.clone()).unwrap();

    let repaired = store.automation_state(&state.consumer).unwrap().unwrap();
    assert_eq!(repaired.generation, 8, "the repair fences older snapshots");
    let members = repaired.members.unwrap();
    assert!(!members.failed.contains_key("moved"));
    assert!(!members.withheld.contains_key("moved"));
    assert_eq!(members.pending["moved"].source, revision("head"));
    assert_eq!(
        members.failed.get("unmoved"),
        state.members.as_ref().unwrap().failed.get("unmoved"),
        "a member shelved at the source it is pending at stays retired"
    );
    assert!(members.withheld.contains_key("unmoved"));

    // Applied once: reopening leaves the repaired state alone.
    compose::automation_store(base.clone()).unwrap();
    assert_eq!(
        store
            .automation_state(&state.consumer)
            .unwrap()
            .unwrap()
            .generation,
        8
    );
}

/// A task-pilot member the member host settled from the checkpoint at
/// apply's former position after a step was inserted before apply: the
/// `pilots` fan-in output held no member evidence, so it was recorded failed,
/// even while the run's real apply was still to come. The one-time repair
/// releases those records; a record whose run checkpointed apply there, or
/// whose run is unknown, stays.
#[test]
fn feature_repair_releases_pilot_failures_settled_from_another_steps_checkpoint() {
    if isolated("feature_repair_releases_pilot_failures_settled_from_another_steps_checkpoint") {
        return;
    }
    use orbit_types::workflow::automation::members::{
        MemberAttempt, MemberState, StateMember, StateTriggerKind,
    };
    use orbit_types::workflow::{JobRunState, PipelineState};
    use serde_json::json;

    let base = Store::open_in_memory().unwrap();
    automation_schema_v3(&base);
    let jobs = compose::workspace_job_run_store(base.clone(), "workspace");
    let run = |steps: &[(u32, &str, serde_json::Value)]| {
        let run = jobs
            .insert_job_run("task_pilot_pipeline", 1, now(), None, None)
            .unwrap();
        let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        for (index, id, output) in steps {
            state.record_step(*index, JobRunState::Success, Some(output.clone()), None);
            state.record_pipeline_output(id, output.clone());
        }
        jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    };
    let pilots = json!([{"partition_index": 0, "tasks": []}]);
    let apply = json!({"member_evidence": [], "repair_count": 1});
    let runs = [
        // Settled from the fan-in while apply had yet to run.
        ("running", run(&[(2, "pilots", pilots.clone())])),
        // Settled from the fan-in; apply and its repair ran afterwards.
        (
            "applied",
            run(&[(2, "pilots", pilots.clone()), (3, "apply", apply.clone())]),
        ),
        // Before the inserted step, apply itself sat at that position.
        ("read", run(&[(2, "apply", apply)])),
        ("unknown", "missing-run".to_string()),
    ];

    let member = |key: &str| StateMember {
        key: key.into(),
        task_ids: vec![key.into()],
        fingerprint: format!("{key}-fingerprint"),
        source: SourceRevision {
            commit: "head".into(),
            tree: "head-tree".into(),
        },
        evidence: json!({"task_id": key}),
        first_seen: now(),
        changed_at: now(),
        crew: None,
    };
    let failed = |key: &str, run_id: &str| MemberAttempt {
        consumer: "fixture/pilot".into(),
        kind: StateTriggerKind::PreparationEligible,
        id: format!("attempt-{key}"),
        member: member(key),
        members: vec![],
        attempt: 1,
        max_attempts: 2,
        deadline: now(),
        retry_after: now(),
        action_key: format!("automation:attempt-{key}:1"),
        action_id: Some(run_id.into()),
        exhausted: true,
    };
    let mut state = baseline();
    state.consumer = "fixture/pilot".into();
    state.trigger = None;
    state.generation = 3;
    state.members = Some(MemberState {
        pending: runs
            .iter()
            .map(|(key, _)| (key.to_string(), member(key)))
            .collect(),
        failed: runs
            .iter()
            .map(|(key, run_id)| (key.to_string(), failed(key, run_id)))
            .collect(),
        withheld: runs
            .iter()
            .map(|(key, _)| (key.to_string(), "no_member_evidence".into()))
            .collect(),
        ..Default::default()
    });
    base.connection()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO automation_consumers VALUES (?1,?2,?3)",
            rusqlite::params![
                state.consumer,
                state.generation,
                serde_json::to_string(&state).unwrap()
            ],
        )
        .unwrap();
    let store = compose::automation_store(base.clone()).unwrap();

    let repaired = store.automation_state(&state.consumer).unwrap().unwrap();
    assert_eq!(repaired.generation, 4, "the repair fences older snapshots");
    let members = repaired.members.unwrap();
    for key in ["running", "applied"] {
        assert!(!members.failed.contains_key(key), "{key} is released");
        assert!(!members.withheld.contains_key(key), "{key} is released");
        assert!(members.pending.contains_key(key), "{key} stays observed");
    }
    for key in ["read", "unknown"] {
        assert_eq!(
            members.failed.get(key),
            state.members.as_ref().unwrap().failed.get(key),
            "{key} keeps its failure record"
        );
        assert!(members.withheld.contains_key(key));
    }

    // Applied once: reopening leaves the repaired state alone.
    compose::automation_store(base.clone()).unwrap();
    assert_eq!(
        store
            .automation_state(&state.consumer)
            .unwrap()
            .unwrap()
            .generation,
        4
    );
}
