//! Inherited delivery evidence through public resume and observation APIs.

use std::path::Path;

use chrono::Duration;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::workflow::{
    CommitObservationStatus, JobRun, JobRunState, JobTargetType, PipelineState,
};
use serde_json::json;

use super::Fixture;

// These fixtures seed host checkpoints, then use public resume admission to
// copy them. Provider output and submitted input cannot manufacture ancestry.
impl Fixture {
    fn delivery_job(&self) {
        let resources = self._root.path().join("home/.orbit/resources");
        let activities = resources.join("activities");
        std::fs::create_dir_all(&activities).unwrap();
        for name in ["git_commit", "pr_complete", "release_locks"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("assets/activities/{name}.yaml")),
                activities.join(format!("{name}.yaml")),
            )
            .unwrap();
        }
        std::fs::write(resources.join("jobs/fixture_delivery.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_delivery\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - id: locks\n      target: activity:release_locks\n    - id: commit\n      target: activity:git_commit\n    - id: complete_pr\n      target: activity:pr_complete\n").unwrap();
    }

    fn save_run(&self, run: &JobRun) {
        self.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&self.runtime.workspace_id().unwrap(), run, None)
            .unwrap();
    }

    fn step(&self, run: &JobRun, index: u32, id: &str) {
        self.runtime
            .complete_job_run_step(
                &run.run_id,
                &JobRunStepParams {
                    lock_wait_ms: None,
                    step_index: index as usize,
                    target_type: JobTargetType::Activity,
                    target_id: id.into(),
                    started_at: run.created_at,
                    finished_at: run.created_at + Duration::seconds(1),
                    duration_ms: Some(1000),
                    exit_code: Some(0),
                    agent_response_json: None,
                    state: JobRunState::Success,
                    error_code: None,
                    error_message: None,
                },
            )
            .unwrap();
    }

    fn audit_step(&self, run: &JobRun, id: &str) {
        use orbit_types::workflow::activity_job::{
            V2AuditEnvelope, V2AuditEvent, V2AuditEventKind,
        };
        for (offset, kind) in [
            V2AuditEventKind::StepStarted { step_id: id.into() },
            V2AuditEventKind::StepFinished {
                step_id: id.into(),
                outcome: "success".into(),
                lock_wait_ms: None,
                error_message: None,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let event = V2AuditEvent {
                envelope: V2AuditEnvelope {
                    schema_version: 1,
                    event_type: kind.event_type().into(),
                    event_id: format!("{}-{id}-{offset}", run.run_id),
                    ts: run.created_at + Duration::seconds(offset as i64),
                    run_id: run.run_id.clone(),
                    agent_identity: "system".into(),
                    parent_event_id: None,
                    workspace_path: None,
                },
                kind,
            };
            self.runtime
                .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                    workspace_id: self.runtime.workspace_id().unwrap(),
                    event_id: event.envelope.event_id.clone(),
                    source: "v2_envelope".into(),
                    schema_version: 1,
                    event_type: event.envelope.event_type.clone(),
                    ts: event.envelope.ts,
                    run_id: run.run_id.clone(),
                    agent_identity: "system".into(),
                    parent_event_id: None,
                    workspace_path: None,
                    payload_json: serde_json::to_string(&event).unwrap(),
                })
                .unwrap();
        }
    }

    fn use_audit_records(&self, run: &JobRun, id: &str) {
        self.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "DELETE FROM job_run_steps WHERE workspace_id = ?1 AND run_id = ?2",
                        [&self.runtime.workspace_id().unwrap(), &run.run_id],
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
        self.audit_step(run, id);
    }

    fn committed_source(&self) -> (JobRun, PipelineState) {
        self.delivery_job();
        let mut source = self.run(
            "source",
            "fixture_delivery",
            1,
            Some(json!({"task_ids": [self.task]})),
        );
        source.state = JobRunState::Failed;
        source.finished_at = Some(source.created_at + Duration::seconds(2));
        self.save_run(&source);
        self.step(&source, 1, "commit");
        let mut state = PipelineState::new(source.run_id.clone(), source.job_id.clone(), json!({}));
        let output = json!({"phase":"commit", "decision":"performed", "committed":true,
            "task_id":self.task, "job_run_id":source.run_id,
            "base_sha":"a".repeat(40), "commit_sha":"b".repeat(40)});
        state.record_step(0, JobRunState::Success, Some(json!({})), None);
        state.record_pipeline_output("locks", json!({}));
        state.record_step(1, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output("commit", output);
        self.runtime
            .write_run_state(&source.run_id, &state)
            .unwrap();
        (source, state)
    }

    fn resume(&self, source: &JobRun) -> JobRun {
        orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
        let result = self
            .runtime
            .submit_resume_run(&source.run_id, Some("test"), None)
            .unwrap();
        self.runtime
            .wait_pipeline_runs(std::slice::from_ref(&result.run_id), 10, 1, Some("test"))
            .unwrap();
        self.runtime.show_job_run_observed(&result.run_id).unwrap()
    }

    fn land(&self, run: &mut JobRun) {
        let mut state = self.runtime.read_run_state(&run.run_id).unwrap().unwrap();
        // The refreshed candidate is separate from the original commit.
        state.record_pipeline_output(
            "sync_base",
            json!({"head_before":"b".repeat(40),
            "head_after":"c".repeat(40)}),
        );
        let output = json!({"phase":"complete", "merge":{"merged":true,
            "pr_number":"4790", "landed_commit":"d".repeat(40)}});
        state.record_step(2, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output("complete_pr", output);
        state.compact_for_terminal(JobRunState::Success);
        self.runtime.write_run_state(&run.run_id, &state).unwrap();
        // Execution records use tail-local indices; logical checkpoints do not.
        self.step(run, 0, "complete_pr");
        run.state = JobRunState::Success;
        self.save_run(run);
    }
}

pub(super) fn public_resume_retains_commit_producer_and_reads_current_landing() {
    let fixture = Fixture::new();
    let (source, original_state) = fixture.committed_source();
    fixture.use_audit_records(&source, "commit");
    let original = fixture
        .runtime
        .observe_task_delivery(&fixture.task, Some(&source.run_id))
        .unwrap();
    assert_eq!(
        original.delivery_status,
        orbit_types::workflow::RunDeliveryStatus::Committed
    );
    assert_eq!(
        original.landing.status,
        orbit_types::workflow::LandingObservationStatus::NotReached
    );
    let mut resumed = fixture.resume(&source);
    fixture.land(&mut resumed);
    fixture.use_audit_records(&resumed, "complete_pr");
    let explicit = fixture
        .runtime
        .observe_task_delivery(&fixture.task, Some(&resumed.run_id))
        .unwrap();
    assert_eq!(
        explicit.delivery_status,
        orbit_types::workflow::RunDeliveryStatus::Landed,
        "{explicit:?}"
    );
    assert_eq!(
        explicit,
        fixture
            .runtime
            .observe_task_delivery(&fixture.task, None)
            .unwrap()
    );
    assert_eq!(explicit.commit.head_sha, Some("b".repeat(40)));
    assert_eq!(
        explicit.commit.observed_at,
        Some(source.created_at + Duration::seconds(1))
    );
    let provenance = serde_json::to_value(&explicit.commit.provenance).unwrap();
    assert_eq!(provenance["source_run_id"], source.run_id);
    assert_eq!(provenance["step_index"], 1);
    assert_eq!(explicit.landing.landed_commit, Some("d".repeat(40)));
    assert_eq!(
        explicit.landing.observed_at,
        Some(resumed.created_at + Duration::seconds(1))
    );
    assert_eq!(
        serde_json::to_value(&explicit.landing.provenance).unwrap()["source_run_id"],
        resumed.run_id
    );
    assert_eq!(
        fixture
            .runtime
            .read_run_state(&source.run_id)
            .unwrap()
            .unwrap(),
        original_state
    );
    assert_eq!(
        fixture
            .runtime
            .observe_task_delivery(&fixture.task, Some(&source.run_id))
            .unwrap(),
        original
    );
}

pub(super) fn inherited_commit_refuses_invalid_parent_evidence() {
    // Each mutation corrupts one observable trust boundary of the reproduced
    // resume incident. The public read must keep refusing despite run success
    // and a plausible deterministic landing checkpoint.
    for case in [
        "missing",
        "cycle",
        "source_cycle",
        "foreign_workspace",
        "cross_job",
        "cross_task",
        "sibling",
        "input_claim",
        "source_bytes",
        "inherited_bytes",
        "source_pipeline",
        "source_state_owner",
        "current_state_owner",
        "source_step",
        "source_definition",
        "source_input",
        "missing_state",
        "source_contract",
        "audit_actor",
        "audit_kind",
        "audit_schema",
        "audit_run",
        "audit_incomplete",
        "audit_bound",
        "source_index",
        "current_pointer",
        "too_deep",
    ] {
        let fixture = Fixture::new();
        let (mut source, mut source_state) = fixture.committed_source();
        let mut resumed = fixture.resume(&source);
        fixture.land(&mut resumed);
        let mut current = fixture
            .runtime
            .read_run_state(&resumed.run_id)
            .unwrap()
            .unwrap();
        match case {
            "missing" => resumed.retry_source_run_id = Some("missing".into()),
            "cycle" => resumed.retry_source_run_id = Some(resumed.run_id.clone()),
            "source_cycle" => {
                source.retry_source_run_id = Some(resumed.run_id.clone());
                fixture.save_run(&source);
            }
            "foreign_workspace" => {
                let store = fixture.runtime.sqlite_store().unwrap();
                store
                    .delete_job_run_for_workspace(
                        &fixture.runtime.workspace_id().unwrap(),
                        &source.run_id,
                    )
                    .unwrap();
                store
                    .upsert_job_run_for_workspace("foreign", &source, None)
                    .unwrap();
            }
            "cross_job" => {
                source.job_id = "fixture_check".into();
                fixture.save_run(&source);
            }
            "cross_task" => {
                source.input = Some(json!({"task_ids":[fixture.other_task]}));
                fixture.save_run(&source);
            }
            "sibling" => {
                let mut sibling = fixture.run(
                    "sibling",
                    "fixture_delivery",
                    2,
                    Some(json!({"task_ids":[fixture.task]})),
                );
                sibling.retry_source_run_id = Some(source.run_id.clone());
                fixture.save_run(&sibling);
                // Bytes name a sibling, while the durable parent still names source.
                current.pipeline["commit"]["job_run_id"] = json!(sibling.run_id);
            }
            "input_claim" => {
                resumed.retry_source_run_id = None;
                resumed.input = Some(
                    json!({"task_ids":[fixture.task], "retry_source_run_id": source.run_id,
                    "result":{"commit":source_state.pipeline["commit"]}}),
                );
            }
            "source_bytes" => {
                source_state.pipeline["commit"]["commit_sha"] = json!("e".repeat(40));
                source_state
                    .step_outputs
                    .insert(1, source_state.pipeline["commit"].clone());
            }
            "inherited_bytes" => {
                current.pipeline["commit"]["commit_sha"] = json!("e".repeat(40));
            }
            "source_pipeline" => {
                source_state.pipeline["commit"] = json!({"forged":true});
            }
            "source_state_owner" => {
                source_state.run_id = "other".into();
            }
            "current_state_owner" => {
                current.job_id = "other".into();
            }
            "source_step" => {
                // A successful unrelated step at the commit execution index is
                // not the commit producer (nor is its provider envelope).
                fixture.step(&source, 1, "unrelated");
            }
            "audit_actor" | "audit_kind" | "audit_schema" | "audit_run" | "audit_incomplete"
            | "audit_bound" => {
                fixture.use_audit_records(&source, "commit");
                fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
                    match case {
                        "audit_kind" => { tx.connection().execute("UPDATE v2_audit_events SET payload_json = json_set(payload_json, '$.body_kind', 'step_started') WHERE run_id = ?1 AND event_type = 'step.finished'", [&source.run_id]).unwrap(); }
                        "audit_schema" => { tx.connection().execute("UPDATE v2_audit_events SET schema_version = 2, payload_json = json_set(payload_json, '$.schemaVersion', 2) WHERE run_id = ?1", [&source.run_id]).unwrap(); }
                        "audit_actor" => { tx.connection().execute("UPDATE v2_audit_events SET agent_identity = 'provider' WHERE run_id = ?1", [&source.run_id]).unwrap(); }
                        "audit_run" => { tx.connection().execute("UPDATE v2_audit_events SET payload_json = json_set(payload_json, '$.run_id', 'foreign') WHERE run_id = ?1", [&source.run_id]).unwrap(); }
                        "audit_incomplete" => { tx.connection().execute("DELETE FROM v2_audit_events WHERE run_id = ?1 AND event_type = 'step.started'", [&source.run_id]).unwrap(); }
                        "audit_bound" => {}
                        _ => unreachable!(),
                    }
                    Ok(())
                }).unwrap();
                if case == "audit_bound" {
                    // Fixed-size corrupt metadata, not a timing/load harness.
                    for index in 0..128 {
                        fixture.audit_step(&source, &format!("unrelated-{index}"));
                    }
                }
            }
            "missing_state" => {
                fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
                    tx.connection().execute("DELETE FROM job_run_states WHERE workspace_id = ?1 AND run_id = ?2",
                        [&fixture.runtime.workspace_id().unwrap(), &source.run_id]).unwrap();
                    Ok(())
                }).unwrap();
            }
            "source_contract" => {
                source_state.pipeline["commit"]["decision"] = json!("provider_claimed");
                source_state
                    .step_outputs
                    .insert(1, source_state.pipeline["commit"].clone());
                current.pipeline["commit"] = source_state.pipeline["commit"].clone();
            }
            "source_definition" | "source_index" | "source_input" => {
                let original = std::fs::read_to_string(
                    fixture
                        ._root
                        .path()
                        .join("home/.orbit/resources/jobs/fixture_delivery.yaml"),
                )
                .unwrap();
                let modified = if case == "source_definition" {
                    original.replace(
                        "target: activity:git_commit",
                        "target: activity:release_locks",
                    )
                } else if case == "source_input" {
                    original.replace(
                        "target: activity:git_commit",
                        "target: activity:git_commit\n      default_input: {allow_empty: true}",
                    )
                } else {
                    original.replace(
                        "    - id: locks\n      target: activity:release_locks\n",
                        "",
                    )
                };
                std::fs::create_dir_all(&fixture.runtime.paths().job_runs_dir).unwrap();
                std::fs::write(
                    fixture.runtime.paths().job_runs_dir.join("source.job.yaml"),
                    modified,
                )
                .unwrap();
            }
            "current_pointer" => {
                current.pipeline["copy"] = current.pipeline["commit"].clone();
                current.step_output_pointers.insert(1, "/copy".into());
            }
            "too_deep" => {
                let mut previous = source.run_id.clone();
                for index in 0..64 {
                    let mut link = fixture.run(
                        &format!("link-{index}"),
                        "fixture_delivery",
                        2 + index,
                        Some(json!({"task_ids":[fixture.task]})),
                    );
                    link.retry_source_run_id = Some(previous);
                    fixture.save_run(&link);
                    let mut state = source_state.clone();
                    state.run_id = link.run_id.clone();
                    fixture
                        .runtime
                        .write_run_state(&link.run_id, &state)
                        .unwrap();
                    previous = link.run_id;
                }
                resumed.retry_source_run_id = Some(previous);
            }
            _ => unreachable!(),
        }
        if !matches!(case, "foreign_workspace" | "missing_state") {
            fixture
                .runtime
                .write_run_state(&source.run_id, &source_state)
                .unwrap();
        }
        fixture
            .runtime
            .write_run_state(&resumed.run_id, &current)
            .unwrap();
        fixture.save_run(&resumed);
        let observed = fixture
            .runtime
            .observe_task_delivery(&fixture.task, Some(&resumed.run_id))
            .unwrap();
        assert_eq!(
            observed.delivery_status,
            orbit_types::workflow::RunDeliveryStatus::Unavailable,
            "{case}: {observed:?}"
        );
        assert_eq!(
            observed.commit.status,
            CommitObservationStatus::Unavailable,
            "{case}: {observed:?}"
        );
        assert!(
            observed.commit.head_sha.is_none(),
            "{case} exposed untrusted commit"
        );
        assert!(
            observed.commit.reason.is_some(),
            "{case} must explain refusal"
        );
    }
}

pub(super) fn repeated_resume_authenticates_each_parent_and_requires_current_landing() {
    let fixture = Fixture::new();
    let (source, _) = fixture.committed_source();
    let mut first = fixture.resume(&source);
    first.state = JobRunState::Failed;
    fixture.save_run(&first);
    let mut second = fixture.resume(&first);
    fixture.land(&mut second);
    let observed = fixture
        .runtime
        .observe_run_delivery(&second.run_id, &fixture.task)
        .unwrap();
    assert_eq!(
        observed.delivery_status,
        orbit_types::workflow::RunDeliveryStatus::Landed
    );
    assert_eq!(
        observed.commit.observed_at,
        Some(source.created_at + Duration::seconds(1))
    );
    assert_eq!(
        serde_json::to_value(&observed.commit.provenance).unwrap()["source_run_id"],
        source.run_id
    );
    // A landing checkpoint alone, even with a successful run, cannot stand in
    // for a current successful deterministic execution record.
    fixture.step(&second, 0, "unrelated");
    let observed = fixture
        .runtime
        .observe_run_delivery(&second.run_id, &fixture.task)
        .unwrap();
    assert_eq!(observed.commit.status, CommitObservationStatus::Committed);
    assert_eq!(
        observed.landing.status,
        orbit_types::workflow::LandingObservationStatus::Unavailable
    );
    assert!(observed.landing.landed_commit.is_none());
}

pub(super) fn legacy_checkpoint_and_wire_shape_remain_readable_after_resume() {
    let fixture = Fixture::new();
    let (source, mut state) = fixture.committed_source();
    // The original contract allowed missing job_run_id. Identical inherited
    // bytes still need a successful producing step and authenticated parents.
    state.pipeline["commit"]
        .as_object_mut()
        .unwrap()
        .remove("job_run_id");
    state
        .step_outputs
        .insert(1, state.pipeline["commit"].clone());
    fixture
        .runtime
        .write_run_state(&source.run_id, &state)
        .unwrap();
    let mut resumed = fixture.resume(&source);
    fixture.land(&mut resumed);
    let observed = fixture
        .runtime
        .observe_run_delivery(&resumed.run_id, &fixture.task)
        .unwrap();
    assert_eq!(
        observed.delivery_status,
        orbit_types::workflow::RunDeliveryStatus::Landed
    );
    assert_eq!(
        observed.commit.observed_at,
        Some(source.created_at + Duration::seconds(1))
    );
    let mut legacy = serde_json::to_value(observed).unwrap();
    for field in ["commit", "landing"] {
        legacy[field]["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("source_run_id");
    }
    let decoded: orbit_types::workflow::RunDeliveryObservation =
        serde_json::from_value(legacy).unwrap();
    assert_eq!(decoded.commit.status, CommitObservationStatus::Committed);
    assert_eq!(decoded.commit.provenance.unwrap().source_run_id, None);
}
