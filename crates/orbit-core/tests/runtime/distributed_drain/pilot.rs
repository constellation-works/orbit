//! Pilot/owner-pull interleavings at the real owner tool boundary.

use super::*;

fn action(pair: &Pair, name: &str, input: Value) -> Value {
    pair.wire
        .owner
        .run_deterministic(name, &json!({}), &input, ToolContext::default())
        .unwrap()
}

fn prepare(pair: &Pair, task_id: &str) -> Value {
    action(
        pair,
        "prepare_task_pilot",
        json!({
            "workspace_path": pair.owner_repo, "task_ids": [task_id], "base_branch": "main",
        }),
    )
}

#[test]
fn owner_pull_defers_an_active_pilot_and_admits_it_after_the_hold_ends() {
    if !isolated(
        module_path!(),
        "owner_pull_defers_an_active_pilot_and_admits_it_after_the_hold_ends",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let prepared = prepare(&pair, &pair.tasks[0]);
    let jobs = orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
    state.record_step(0, JobRunState::Success, Some(prepared), None);
    pair.wire
        .owner
        .write_run_state(&run.run_id, &state)
        .unwrap();

    let drain = pair.start_drain();
    let first_leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[1]);
    let records = pair.follower_jobs.local_pull_admissions().unwrap();
    let receipt = records
        .iter()
        .filter_map(|record| record.receipt.as_ref())
        .find(|receipt| {
            receipt
                .claim
                .as_ref()
                .is_some_and(|claim| claim.task_id == pair.tasks[1])
        })
        .unwrap();
    assert_eq!(
        receipt.queue_depth, 0,
        "the remaining queue excludes the piloted task"
    );
    assert!(
        receipt.deferred_conflicts.iter().any(|entry| {
            entry.task_id == pair.tasks[0] && entry.reason.contains("active task-pilot preparation")
        }),
        "{receipt:?}"
    );

    jobs.finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
    pair.leaf_fails_with(&first_leaf, "candidate validation failed");
    let second = pair.pass(&drain);
    assert!(launch_refused(&second), "{second}");
    assert!(
        pair.owner_claims()
            .iter()
            .any(|entry| entry["claim"]["task_id"] == pair.tasks[0])
    );
}

#[test]
fn follower_claim_supersedes_a_prepared_assessment_without_unscoped_writes() {
    if !isolated(
        module_path!(),
        "follower_claim_supersedes_a_prepared_assessment_without_unscoped_writes",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let prepared = prepare(&pair, &pair.tasks[0]);
    let drain = pair.start_drain();
    let leaf = pair.queued_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), pair.tasks[0]);
    let before = pair.owner_task(&pair.tasks[0]);
    let output = action(
        &pair,
        "apply_task_pilot_results",
        json!({
            "workspace_path": pair.owner_repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": pair.tasks,
                "tasks": [{"task_id": pair.tasks[0], "context_files_before": ["file:src/f0.rs"]}]}],
        }),
    );
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["outcome"], "superseded");
    assert_eq!(output["task_outcomes"][0]["reason"], "execution_claim");
    assert_eq!(output["applied_count"], 0);
    assert_eq!(output["unresolved_count"], 0);
    assert_eq!(pair.owner_task(&pair.tasks[0]), before);
    assert_eq!(
        action(&pair, "pipeline_success_guard", json!({"result": output}))["succeeded"],
        true
    );
}

fn apply_validation_assessment(pair: &Pair, criterion: &str) {
    apply_assessment(pair, criterion, json!({}));
}

#[test]
fn owner_pull_holds_host_operational_until_a_decision_or_newer_assessment() {
    if !isolated(
        module_path!(),
        "owner_pull_holds_host_operational_until_a_decision_or_newer_assessment",
    ) {
        return;
    }
    for release in ["approve-anyway", "selectors"] {
        let pair = Pair::new(1);
        apply_assessment(
            &pair,
            "The owner-side repair is observable.",
            json!({
                "disposition": "host_operational", "context_files_after": [],
                "evidence": "Only the operator can repair the owner-side definition.",
            }),
        );
        let drain = pair.start_drain();
        let first = pair.pass(&drain);
        assert!(first["error"].is_null(), "{first}");
        assert!(pair.owner_claims().is_empty());
        assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
        let records = pair.follower_jobs.local_pull_admissions().unwrap();
        let receipt = records
            .iter()
            .filter_map(|record| record.receipt.as_ref())
            .next()
            .unwrap();
        assert_eq!(receipt.queue_depth, 0);
        assert!(
            receipt
                .deferred_conflicts
                .iter()
                .any(|entry| entry.task_id == pair.tasks[0]
                    && entry.reason.contains("host_operational")
                    && entry
                        .reason
                        .contains("Only the operator can repair the owner-side definition.")),
            "{receipt:?}"
        );
        let history = pair.wire.owner.get_task_history(&pair.tasks[0]).unwrap();
        let hold = history
            .iter()
            .find(|entry| entry.event == "host_operational_held")
            .unwrap();
        let record: Value = serde_json::from_str(hold.note.as_deref().unwrap()).unwrap();
        assert_eq!(record["disposition"], "host_operational");
        if release == "selectors" {
            apply_assessment(&pair, "The owner-side repair is observable.", json!({}));
        } else {
            pair.wire.owner.update_task_as_human(&pair.tasks[0], orbit_core::application::task::TaskUpdateParams {
                comment: Some("task-pilot-admission: approve-anyway\nThe operator reviewed the handoff and accepts admission.".into()),
                ..Default::default()
            }, "human:fixture".into()).unwrap();
        }
        let second = pair.pass(&drain);
        assert!(launch_refused(&second), "{second}");
        assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[0]);
    }
}

#[test]
fn owner_pull_admits_selectors_and_verified_no_diff_dispositions() {
    if !isolated(
        module_path!(),
        "owner_pull_admits_selectors_and_verified_no_diff_dispositions",
    ) {
        return;
    }
    for disposition in ["selectors", "verified_no_diff"] {
        let pair = Pair::new(1);
        let findings = if disposition == "verified_no_diff" {
            json!({"disposition": disposition, "context_files_after": [], "evidence": "The operator verified that no repository edit is needed."})
        } else {
            json!({})
        };
        apply_assessment(&pair, "The repair is observable.", findings);
        let drain = pair.start_drain();
        let first = pair.pass(&drain);
        assert!(launch_refused(&first), "{first}");
        assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[0]);
    }
}

/// Re-scope the task to `criterion` and apply one assessment of it carrying
/// the agent findings in `findings`.
pub(super) fn apply_assessment(pair: &Pair, criterion: &str, findings: Value) {
    let task_id = &pair.tasks[0];
    pair.wire
        .owner
        .update_task_as_human(
            task_id,
            orbit_core::application::task::TaskUpdateParams {
                acceptance_criteria: Some(vec![criterion.into()]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let prepared = prepare(pair, task_id);
    let mut assessment = json!({
        "task_id": task_id, "context_files_before": ["file:src/f0.rs"],
        "context_files_after": ["file:src/f0.rs"], "disposition": "selectors",
        "recommended_crew": "fixture", "recommended_complexity": "low",
        "confidence": "high", "assessment_rationale": "The declared file contains the repair.",
        "validation_approach": "Exercise the owner admission boundary.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    for (field, finding) in findings.as_object().unwrap() {
        assessment[field] = finding.clone();
    }
    let output = action(
        pair,
        "apply_task_pilot_results",
        json!({
            "workspace_path": pair.owner_repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task_id], "tasks": [assessment]}],
        }),
    );
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
}

#[test]
fn owner_pull_holds_operator_validation_until_the_operator_resolves_it() {
    if !isolated(
        module_path!(),
        "owner_pull_holds_operator_validation_until_the_operator_resolves_it",
    ) {
        return;
    }
    for decision in ["evaluated", "approve-anyway", "rescope"] {
        let pair = Pair::new(1);
        apply_validation_assessment(&pair, "Run a live evaluation with `orbit.pipeline.invoke`.");
        let history = pair.wire.owner.get_task_history(&pair.tasks[0]).unwrap();
        assert!(history.iter().any(|entry| entry.from_status
            == Some(orbit_types::task::TaskStatus::Proposed)
            && entry.to_status == Some(orbit_types::task::TaskStatus::Backlog)
            && entry.by == "human:fixture"));
        let hold = history
            .iter()
            .find(|entry| entry.event == "operator_validation_held")
            .unwrap();
        let evidence: Value = serde_json::from_str(hold.note.as_deref().unwrap()).unwrap();
        assert_eq!(evidence["hold"]["requirements"][0]["criterion"], 1);
        assert_eq!(
            evidence["hold"]["requirements"][0]["tool"],
            "orbit.pipeline.invoke"
        );
        let drain = pair.start_drain();
        let first = pair.pass(&drain);
        assert!(first["error"].is_null(), "{first}");
        assert!(pair.owner_claims().is_empty());
        assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
        let records = pair.follower_jobs.local_pull_admissions().unwrap();
        let receipt = records
            .iter()
            .filter_map(|record| record.receipt.as_ref())
            .next()
            .unwrap();
        assert_eq!(receipt.queue_depth, 0);
        assert!(
            receipt.deferred_conflicts.iter().any(|entry| {
                entry.task_id == pair.tasks[0]
                    && entry
                        .reason
                        .contains("criterion 1 requires `orbit.pipeline.invoke`")
            }),
            "{receipt:?}"
        );
        let mut resolution = orbit_core::application::task::TaskUpdateParams::default();
        if decision == "rescope" {
            resolution.acceptance_criteria = Some(vec![
                "Observe the repair through the runtime fixture.".into(),
            ]);
        } else {
            resolution.comment = Some(format!(
                "task-pilot-admission: {decision}\nThe operator completed the required evaluation and attached operator-evaluation.json."
            ));
        }
        if decision == "evaluated" {
            resolution.upsert_artifacts = vec![TaskArtifact::from_text(
                "operator-evaluation.json",
                json!({"criterion": 1, "tool": "orbit.pipeline.invoke", "outcome": "passed"})
                    .to_string(),
            )];
        }
        pair.wire
            .owner
            .update_task_as_human(&pair.tasks[0], resolution, "human:fixture".into())
            .unwrap();
        let second = pair.pass(&drain);
        assert!(launch_refused(&second), "{second}");
        assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[0]);
    }
}

/// What the follower's drain recorded of the owner's idle answer for `task`:
/// its reason code and the owner's sentence [ORB-15278].
fn recorded_wait(pair: &Pair, drain: &str, task: &str) -> (String, String) {
    let recorded = pair
        .follower
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .expect("the pass is recorded");
    recorded
        .deferred
        .iter()
        .chain(&recorded.excluded)
        .find(|waiting| waiting.task_id == task)
        .map(|waiting| {
            (
                waiting.reason.clone().unwrap_or_default(),
                waiting.detail.clone().unwrap_or_default(),
            )
        })
        .unwrap_or_else(|| panic!("{task} is not recorded as waiting: {recorded:#?}"))
}

#[test]
fn a_native_os_finding_tags_the_task_so_a_follower_of_another_os_does_not_claim_it() {
    if !isolated(
        module_path!(),
        "a_native_os_finding_tags_the_task_so_a_follower_of_another_os_does_not_claim_it",
    ) {
        return;
    }
    let mut pair = Pair::new(1);
    apply_assessment(
        &pair,
        "A real macOS `sandbox-exec` launch of the fixture succeeds.",
        json!({"required_os": [{
            "criterion": 1, "os": "macos",
            "evidence": "The criterion requires a native macOS sandbox-exec launch.",
        }]}),
    );
    // The untagged task now routes by the tag the pilot added.
    let task = pair.wire.owner.get_task(&pair.tasks[0]).unwrap();
    assert!(
        task.tags.contains(&"os:macos".to_string()),
        "{:?}",
        task.tags
    );
    pair.follower = pair
        .follower
        .clone()
        .with_host_os(Some(orbit_types::task::HostOs::Linux));
    let drain = pair.start_drain();
    let first = pair.pass(&drain);
    assert!(first["error"].is_null(), "{first}");
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    let (reason, detail) = recorded_wait(&pair, &drain, &pair.tasks[0]);
    assert_eq!(reason, "host_os_mismatch", "{detail}");
    assert!(detail.contains("os:macos"), "{detail}");

    // A follower that can produce the evidence is handed the task.
    pair.follower = pair
        .follower
        .clone()
        .with_host_os(Some(orbit_types::task::HostOs::Macos));
    let mac_drain = pair.start_drain();
    pair.pass(&mac_drain);
    assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[0]);
}

#[test]
fn a_machine_finding_keeps_the_task_off_every_other_machine() {
    if !isolated(
        module_path!(),
        "a_machine_finding_keeps_the_task_off_every_other_machine",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let criterion =
        "Timings measured on the owner against its live task store reach a p95 under 1 s.";
    apply_assessment(
        &pair,
        criterion,
        json!({"required_machine": [{
            "criterion": 1, "machine": OWNER,
            "evidence": "Only the owner's live store holds the measured workspace.",
        }]}),
    );
    // A machine requirement has no tag: routing is the owner's hold alone.
    let task = pair.wire.owner.get_task(&pair.tasks[0]).unwrap();
    assert!(
        !task.tags.iter().any(|tag| tag.starts_with("os:")),
        "{:?}",
        task.tags
    );
    let drain = pair.start_drain();
    let first = pair.pass(&drain);
    assert!(first["error"].is_null(), "{first}");
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    let (reason, detail) = recorded_wait(&pair, &drain, &pair.tasks[0]);
    assert_eq!(reason, "owner_hold", "{detail}");
    assert!(
        detail.starts_with("Machine requirement: criterion 1")
            && detail.contains(&format!("only machine {OWNER}"))
            && detail.contains(&format!("machine {FOLLOWER} cannot satisfy it")),
        "{detail}"
    );

    // Re-scoping the criterion releases the hold.
    pair.wire
        .owner
        .update_task_as_human(
            &pair.tasks[0],
            orbit_core::application::task::TaskUpdateParams {
                acceptance_criteria: Some(vec![
                    "The freshness statement's query plan searches tags by task id.".into(),
                ]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    pair.pass(&drain);
    assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[0]);
}

#[test]
fn owner_pull_ignores_stale_validation_and_advisory_transport_findings() {
    if !isolated(
        module_path!(),
        "owner_pull_ignores_stale_validation_and_advisory_transport_findings",
    ) {
        return;
    }
    for criterion in [
        "Call `proc.spawn` over MCP to validate the fixture.",
        "Read the fixture through `orbit.task.eligible`.",
        "An agent calling `orbit.pipeline.invoke` receives the expected denial.",
        "Run the current scenario with `orbit.pipeline.invoke`.",
    ] {
        let pair = Pair::new(1);
        apply_validation_assessment(&pair, criterion);
        if criterion.starts_with("Run") {
            pair.wire
                .owner
                .update_task_as_human(
                    &pair.tasks[0],
                    orbit_core::application::task::TaskUpdateParams {
                        acceptance_criteria: Some(vec![
                            "Run a revised scenario with `orbit.pipeline.invoke`.".into(),
                        ]),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
        }
        let drain = pair.start_drain();
        let pass = pair.pass(&drain);
        assert!(launch_refused(&pass), "{pass}");
        assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[0]);
    }
}
