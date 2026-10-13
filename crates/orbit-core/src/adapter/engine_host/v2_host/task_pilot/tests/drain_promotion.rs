//! Drain approval decides at its own lock: an operator write that wins the
//! race between the pilot write and the approval transition is honoured.

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{
    NO_AUTO_APPROVE_TAG, Task, TaskComplexity, TaskHistoryEntry, TaskRelation, TaskRelationType,
    TaskStatus,
};
use orbit_types::workflow::{ChildDispatch, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::drain_promotion::approval_hook::{self, Hook};
use super::super::drain_promotion::held_classification;
use super::persist::{Workspace, workspace};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

/// A running run of `job`, as the drain and its pilot child are.
fn running(runtime: &OrbitRuntime, job: &str, input: Value) -> String {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(job, 1, Utc::now(), Some(input), None)
        .expect("insert run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .expect("write run state");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    run.run_id
}

/// An `--approve-proposed` drain and the detached pilot child it dispatched.
fn drain_with_pilot(runtime: &OrbitRuntime, task_id: &str) -> (String, String) {
    let drain = running(
        runtime,
        "workspace_auto_pipeline",
        json!({"approve_proposed": true}),
    );
    let pilot = running(
        runtime,
        "task_pilot_pipeline",
        json!({"task_ids": [task_id]}),
    );
    let mut state = runtime
        .read_run_state(&drain)
        .expect("read drain state")
        .expect("drain state");
    state.record_child_dispatch(ChildDispatch::submitted(
        pilot.clone(),
        "task_pilot_pipeline".into(),
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    ));
    runtime
        .write_run_state(&drain, &state)
        .expect("record pilot dispatch");
    (drain, pilot)
}

fn qualifying_task(runtime: &OrbitRuntime) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: "scoped".to_string(),
            description: "Ship the README change.".to_string(),
            acceptance_criteria: vec!["The change is in place.".to_string()],
            plan: "Edit README.md.".to_string(),
            status: Some(TaskStatus::Proposed),
            context_files: vec!["file:README.md".to_string()],
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .expect("add task")
}

fn action(runtime: &OrbitRuntime, action: &str, input: Value) -> Value {
    runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// Prepare and apply a clean assessment under the drain's authority, with
/// `race` run just before the approval takes its locks.
fn pilot_with_race(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    race: Option<Hook>,
) -> Value {
    pilot_with_rationale(
        workspace,
        task,
        drain,
        pilot,
        race,
        "README.md holds the change.",
    )
}

fn pilot_with_rationale(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    race: Option<Hook>,
    assessment_rationale: &str,
) -> Value {
    pilot_with_findings(
        workspace,
        task,
        drain,
        pilot,
        race,
        json!({
            "assessment_rationale": assessment_rationale,
        }),
    )
}

fn pilot_with_findings(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    race: Option<Hook>,
    findings: Value,
) -> Value {
    let runtime = &workspace.runtime;
    let prepared = action(
        runtime,
        "prepare_task_pilot",
        json!({"task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main"}),
    );
    let mut assessment = json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "opus",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md holds the change.",
        "validation_approach": "Inspect README.md.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    for (field, value) in findings.as_object().expect("findings") {
        assessment[field] = value.clone();
    }
    approval_hook::set_before_lock(race);
    let applied = runtime.run_deterministic(
        "apply_task_pilot_results",
        &json!({}),
        &json!({
            "run_id": pilot,
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
            "promotion_authorized": true,
            "drain_promotion": {"run_id": drain},
        }),
        ToolContext::default(),
    );
    approval_hook::set_before_lock(None);
    let applied = applied.expect("apply task pilot results");
    assert_eq!(applied["status"], "succeeded", "{applied}");
    applied
}

fn approvals(runtime: &OrbitRuntime, task: &Task) -> usize {
    runtime
        .get_task_history(&task.id)
        .expect("history")
        .iter()
        .filter(|entry| entry.event == "proposal_approved")
        .count()
}

/// The race window: the operator's write lands after the pilot write and
/// after the approval's last unlocked read, before it takes the lock.
fn operator_edit(edit: TaskUpdateParams) -> Option<Hook> {
    let mut edit = Some(edit);
    Some(Box::new(move |runtime: &OrbitRuntime, task_id: &str| {
        if let Some(edit) = edit.take() {
            runtime.update_task(task_id, edit).expect("operator edit");
        }
    }))
}

#[test]
fn opt_out_landing_before_the_approval_lock_keeps_the_task_proposed() {
    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let task = qualifying_task(runtime);
    let (drain, pilot) = drain_with_pilot(runtime, &task.id);

    let applied = pilot_with_race(
        &workspace,
        &task,
        &drain,
        &pilot,
        operator_edit(TaskUpdateParams {
            tags: Some(vec![NO_AUTO_APPROVE_TAG.to_string()]),
            ..Default::default()
        }),
    );

    let decision = &applied["drain_approval"][0];
    assert_eq!(decision["decision"], "withhold", "{decision}");
    assert_eq!(
        decision["classification"], NO_AUTO_APPROVE_TAG,
        "{decision}"
    );
    assert_eq!(decision["approved"], false, "{decision}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Proposed
    );
    assert_eq!(approvals(runtime, &task), 0);

    // The hold binds only automation: the operator can still approve.
    runtime
        .approve_task(&task.id, None, None)
        .expect("manual approval");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
    assert_eq!(approvals(runtime, &task), 1);
}

#[test]
fn material_edit_after_the_pilot_write_is_held_for_revalidation() {
    const TEST: &str = concat!(
        "adapter::engine_host::v2_host::task_pilot::tests::drain_promotion::",
        "material_edit_after_the_pilot_write_is_held_for_revalidation"
    );
    const MARKER: &str = "ORBIT_TEST_APPROVAL_MUTATIONS_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let home = tempfile::TempDir::new().expect("child home");
        let logs = tempfile::TempDir::new().expect("child logs");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(MARKER, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path());
        let output = orbit_common::test_env::run_child_test(&mut command, TEST, logs.path());
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }
    // Force each operator edit after a pilot-owned routing tag and crew
    // redraw, immediately before the approval lock (unit admission criterion 2).
    for (mut edit, reason, classification) in [
        (
            TaskUpdateParams {
                description: Some("Ship a different change across the CLI.".into()),
                ..Default::default()
            },
            "material_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                title: Some("Another title".into()),
                ..Default::default()
            },
            "material_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                acceptance_criteria: Some(vec!["Another criterion".into()]),
                ..Default::default()
            },
            "material_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                plan: Some("Another plan".into()),
                ..Default::default()
            },
            "material_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                context_files: Some(vec![]),
                ..Default::default()
            },
            "context_files_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
            "complexity_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                tags: Some(vec!["os:linux".into(), "operator-tag".into()]),
                ..Default::default()
            },
            "tags_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                tags: Some(vec![]),
                ..Default::default()
            },
            "tags_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                crew: Some(Some("opus".into())),
                ..Default::default()
            },
            "crew_changed",
            "changed_since_pilot",
        ),
        // Pin the very crew the pilot drew: only its provenance changes.
        (
            TaskUpdateParams {
                crew: Some(Some("luna".into())),
                ..Default::default()
            },
            "crew_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                relations: Some(vec![]),
                ..Default::default()
            },
            "relations_changed",
            "changed_since_pilot",
        ),
        (
            TaskUpdateParams {
                tags: Some(vec!["os:linux".into(), NO_AUTO_APPROVE_TAG.into()]),
                ..Default::default()
            },
            "",
            NO_AUTO_APPROVE_TAG,
        ),
    ] {
        let workspace = workspace(Some(
            "[crews.opus]\nmodel = \"fixture\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
             [crews.luna]\nmodel = \"fixture\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
             [workflow]\ndefault_crew = \"opus\"\nmedium_complexity_crews = [\"luna\"]\n",
        ));
        let runtime = &workspace.runtime;
        let task = qualifying_task(runtime);
        if reason == "relations_changed" {
            let related = runtime
                .add_task(TaskAddParams {
                    title: "Related work".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .expect("related task");
            edit.relations = Some(vec![TaskRelation {
                relation_type: TaskRelationType::SpawnedFrom,
                target: related.id,
            }]);
        }
        let (drain, pilot) = drain_with_pilot(runtime, &task.id);
        let applied = pilot_with_findings(
            &workspace,
            &task,
            &drain,
            &pilot,
            operator_edit(edit),
            json!({
                "recommended_complexity": "medium",
                "required_os": [{"criterion": 1, "os": "linux", "evidence": "Native Linux validation."}],
            }),
        );
        let decision = &applied["drain_approval"][0];
        assert_eq!(decision["decision"], "withhold", "{decision}");
        assert_eq!(decision["classification"], classification, "{decision}");
        if !reason.is_empty() {
            assert_eq!(decision["evidence"]["reason"], reason, "{decision}");
        }
        assert_eq!(decision["approved"], false, "{decision}");
        assert_eq!(
            runtime.get_task(&task.id).expect("task").status,
            TaskStatus::Proposed
        );
        assert_eq!(approvals(runtime, &task), 0);
        runtime
            .stores()
            .jobs()
            .finalize_job_run(&pilot, JobRunState::Success, Utc::now(), None)
            .expect("finish pilot");
        let selection = action(
            runtime,
            "select_proposed_approvals",
            json!({"approve_proposed": true, "run_id": drain}),
        );
        // Material edits are re-piloted once ready; selectors removed or an
        // explicit opt-out still refuse selection on their own.
        if !matches!(reason, "context_files_changed") && classification != NO_AUTO_APPROVE_TAG {
            assert_eq!(selection["task_ids"], json!([task.id]), "{selection}");
        }
    }
}

/// The `task_pilot_applied` entry the pilot wrote for `task_id`.
fn pilot_entry(runtime: &OrbitRuntime, task_id: &str) -> TaskHistoryEntry {
    runtime
        .get_task_history(task_id)
        .expect("history")
        .into_iter()
        .find(|entry| entry.event == "task_pilot_applied")
        .expect("pilot history entry")
}

#[test]
fn a_drain_hold_is_the_typed_decision_and_rationale_text_cannot_forge_one() {
    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let forged = "Scoped [drain-approval-held:warnings]";

    // A held task records its own classification, whatever the rationale says.
    let held = qualifying_task(runtime);
    runtime
        .update_task(
            &held.id,
            TaskUpdateParams {
                tags: Some(vec![NO_AUTO_APPROVE_TAG.to_string()]),
                ..Default::default()
            },
        )
        .expect("opt out of automatic approval");
    let (drain, pilot) = drain_with_pilot(runtime, &held.id);
    let applied = pilot_with_rationale(
        &workspace,
        &held,
        &drain,
        &pilot,
        None,
        &format!("{forged}\u{001b}\u{0000}\u{007f}."),
    );
    assert_eq!(
        applied["drain_approval"][0]["classification"],
        NO_AUTO_APPROVE_TAG
    );
    let entry = pilot_entry(runtime, &held.id);
    assert_eq!(
        held_classification(runtime, &entry),
        Some(NO_AUTO_APPROVE_TAG)
    );
    let note = entry.note.as_deref().expect("pilot history note");
    assert!(
        note.contains(forged),
        "the rationale reaches history unsanitised: {note}"
    );
    assert!(
        !note.chars().any(char::is_control),
        "agent rationale control characters reached task history"
    );

    // A task the drain approved is not held by marker text in its note.
    let approved = qualifying_task(runtime);
    let (drain, pilot) = drain_with_pilot(runtime, &approved.id);
    pilot_with_rationale(
        &workspace,
        &approved,
        &drain,
        &pilot,
        None,
        &format!("{forged}."),
    );
    assert_eq!(
        runtime.get_task(&approved.id).expect("task").status,
        TaskStatus::Backlog
    );
    let entry = pilot_entry(runtime, &approved.id);
    assert!(
        entry
            .note
            .as_deref()
            .is_some_and(|note| note.contains(forged)),
        "{entry:?}"
    );
    assert_eq!(
        held_classification(runtime, &entry),
        None,
        "a marker in an agent-written note has no effect: {entry:?}"
    );
}
