//! Completion recovery through the production CLI resume, engine actions and
//! real task/run stores. Only the remote forge and prior worktree are fixtures.
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use orbit_common::{process::run_bounded_capped, test_env};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryRequest, FinalRecoveryTaskRevision,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, execute_deterministic_action};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRunState, PipelineState};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::unix::Fixture;

const JOB: &str = "resume_completion_fixture";

struct Delivery {
    cli: Fixture,
    runtime: OrbitRuntime,
    db: Connection,
    task: String,
    run: String,
    input: Value,
}

impl Delivery {
    fn new() -> Self {
        let cli = Fixture::new();
        let runtime =
            OrbitRuntime::from_roots(&cli.home.join(".orbit"), &cli.repo.join(".orbit")).unwrap();
        let db = Connection::open(cli.home.join(".orbit/orbit.db")).unwrap();
        let task = cli.json(&[
            "task",
            "add",
            "--title",
            "Recover delivery",
            "--description",
            "Exercise completion recovery.",
            "--acceptance-criteria",
            "Delivery completes.",
            "--plan",
            "Implement and deliver the change.",
            "--complexity",
            "low",
            "--status",
            "backlog",
            "--json",
        ])["id"]
            .as_str()
            .unwrap()
            .to_string();
        let input = json!({"task_ids": [task], "completion": "done", "crew": "sol"});
        let run = "jrun-completion-source".to_string();
        insert_run(&db, &runtime.workspace_id().unwrap(), &run, None, &input);
        let input = json!({
            "task_ids": [task], "completed_task_ids": [task], "completion": "done", "crew": "sol",
            "job_run_id": run, "workspace_path": cli.repo, "pr_number": "42",
            "head": "orbit/candidate", "published_head_sha": "candidate-sha", "base": "agent-main"
        });
        // The submitted run's identity is unavailable until admission allocated it.
        // Install the same worktree output that production checkpoint reuse carries.
        let mut state = PipelineState::new(run.clone(), JOB.into(), input.clone());
        let worktree = json!({"job_run_id": run, "workspace_path": cli.repo});
        state.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
        state.record_pipeline_output("worktree", worktree);
        let implementation = json!({"implemented": true});
        state.record_step(1, JobRunState::Success, Some(implementation.clone()), None);
        state.record_pipeline_output("implement", implementation);
        state.record_step(3, JobRunState::Success, Some(Value::Null), None);
        state.record_pipeline_output("promote_no_diff", Value::Null);
        runtime.write_run_state(&run, &state).unwrap();
        runtime.apply_task_automation_update(&task, TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress), job_run_id: Some(run.clone()),
            execution_summary: Some("Outcome: success\nImplemented the delivery fixture and validated its output.".into()),
            ..Default::default()
        }).unwrap();
        // Inline host actions make this a deterministic job; resumed steps before
        // completion must reuse their checkpoints rather than executing the sentinels.
        let job = json!({"schemaVersion":2,"kind":"Job","metadata":{"name":JOB},"spec":{
            "state":"enabled","kind":"workflow","steps":[
                {"id":"worktree","default_input":{"result":{"status":"failed","run_id":"replayed-worktree"}},"spec":{"type":"deterministic","action":"pipeline_success_guard","config":{}}},
                {"id":"implement","default_input":{"result":{"status":"failed","run_id":"replayed-implementation"}},"spec":{"type":"deterministic","action":"pipeline_success_guard","config":{}}},
                {"id":"promote_tasks","default_input":input,"spec":{"type":"deterministic","action":"pr_promote","config":{}}},
                {"id":"promote_no_diff","when":"false","spec":{"type":"deterministic","action":"pr_promote","config":{}}},
                {"id":"complete_pr","default_input":input,"spec":{"type":"deterministic","action":"pr_complete","config":{}}}
            ]
        }});
        fs::write(
            cli.home
                .join(".orbit/resources/jobs")
                .join(format!("{JOB}.yaml")),
            serde_json::to_string_pretty(&job).unwrap(),
        )
        .unwrap();
        Self {
            cli,
            runtime,
            db,
            task,
            run,
            input,
        }
    }

    fn action(&self, action: &str) -> Result<Value, orbit_common::OrbitError> {
        execute_deterministic_action(
            &self.runtime,
            action,
            &json!({}),
            &self.input,
            false,
            &HashMap::new(),
            None,
        )
    }

    fn promote(&self) {
        // No base-obsolescence check is needed for this already-published fixture.
        let mut input = self.input.clone();
        input.as_object_mut().unwrap().remove("base");
        let output = execute_deterministic_action(
            &self.runtime,
            "pr_promote",
            &json!({}),
            &input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(self.status(), TaskStatus::Review);
        let mut state = self.runtime.read_run_state(&self.run).unwrap().unwrap();
        state.record_step(2, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output("promote_tasks", output);
        self.runtime.write_run_state(&self.run, &state).unwrap();
    }

    /// The early-implementation shape: promotion is not a reused checkpoint,
    /// so a lineage block readmits `in-progress` rather than restoring review.
    fn skip_promotion(&self) {
        let mut state = self.runtime.read_run_state(&self.run).unwrap().unwrap();
        state.step_states.insert(2, JobRunState::Skipped);
        state.step_outputs.remove(&2);
        self.runtime.write_run_state(&self.run, &state).unwrap();
        let path = self
            .cli
            .home
            .join(".orbit/resources/jobs")
            .join(format!("{JOB}.yaml"));
        let mut job: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        job["spec"]["steps"][2]["when"] = json!("false");
        fs::write(path, job.to_string()).unwrap();
    }

    /// A system failure-handoff block. `note` must carry the producer's `: run=<id>,` field.
    fn block_handoff(&self, event: &str, note: &str) {
        self.runtime
            .apply_task_automation_update(
                &self.task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Blocked),
                    status_event: Some(event.to_string()),
                    status_note: Some(note.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(self.status(), TaskStatus::Blocked);
    }

    fn block(&self, run: &str) {
        self.runtime
            .apply_final_recovery(
                &FinalRecoveryRequest {
                    task_id: self.task.clone(),
                    run_id: run.into(),
                    observed: FinalRecoveryTaskRevision::of(
                        &self.runtime.get_task(&self.task).unwrap(),
                    ),
                    repo_root: self.cli.repo.clone(),
                    base_ref: "agent-main".into(),
                    completion: FinalRecoveryCompletion::Done,
                    requeue_bound: Default::default(),
                },
                Some(&json!({"decision":"escalate", "diagnosis":"Completion cannot proceed yet.", "human_action":"Resume after checks pass."})),
            )
            .unwrap();
        assert_eq!(self.status(), TaskStatus::Blocked);
    }

    fn fail(&self, run: &str) {
        self.db
            .execute(
                "UPDATE job_runs SET state='failed', finished_at=?2 WHERE run_id=?1",
                params![run, Utc::now().to_rfc3339()],
            )
            .unwrap();
    }

    fn status(&self) -> TaskStatus {
        self.runtime.get_task(&self.task).unwrap().status
    }

    fn resume(&self, source: &str, expected: &str) -> String {
        let submitted = self.cli.json(&["job", "resume", source, "--json"]);
        let run = submitted["run_id"].as_str().unwrap().to_string();
        self.cli.poll_run(&run, expected, Duration::from_secs(20));
        assert_eq!(
            self.runtime
                .show_job_run(&run)
                .unwrap()
                .retry_source_run_id
                .as_deref(),
            Some(source)
        );
        run
    }
}

fn insert_run(db: &Connection, workspace: &str, id: &str, parent: Option<&str>, input: &Value) {
    let now = Utc::now().to_rfc3339();
    db.execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,finished_at,created_at,retry_source_run_id) VALUES (?1,?2,?3,1,'failed',?4,?5,?5,?5,?5,?6)", params![id,workspace,JOB,input.to_string(),now,parent]).unwrap();
}

fn isolated(name: &str) -> bool {
    const MARKER: &str = "ORBIT_RESUME_LIFECYCLE_CHILD";
    if std::env::var(MARKER).ok().as_deref() == Some(name) {
        return true;
    }
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let qualified = format!("job_resume_detached::lifecycle::{name}");
    command
        .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
        .env(MARKER, name)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = run_bounded_capped(&mut command, Duration::from_secs(120), 256 * 1024).unwrap();
    test_env::assert_child_test_passed(&qualified, output.status, &output.stdout, &output.stderr);
    false
}

/// A shell forge substitute returns exactly the pinned PR. There is no network
/// or merge mutation; switching its response models checks finishing elsewhere.
fn forge(root: &std::path::Path, merged: bool) {
    let bin = root.join("bin");
    let install = !bin.exists();
    fs::create_dir_all(&bin).unwrap();
    let response = root.join("pr.json");
    fs::write(&response, json!({
        "state": if merged {"MERGED"} else {"OPEN"},
        "mergeStateStatus": "BLOCKED", "reviewDecision": "CHANGES_REQUESTED",
        "headRefName": "orbit/candidate", "headRefOid": "candidate-sha", "baseRefName": "agent-main",
        "mergeCommit": if merged {json!({"oid":"landed-sha"})} else {Value::Null},
        "mergedAt": if merged {json!("2026-10-05T00:00:00Z")} else {Value::Null},
        "statusCheckRollup": []
    }).to_string()).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, format!("#!/bin/sh\ncase \"$1 $2\" in\n 'pr view') cat '{}' ;;\n 'pr list') echo '[]' ;;\n *) echo \"Unexpected forge mutation: $*\" >&2; exit 1 ;;\nesac\n", response.display())).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    // Install once in the isolated child before any runtime or worker starts.
    // Later forge responses change only the file, never the process environment.
    if install {
        unsafe {
            std::env::set_var(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        }
    }
}

#[test]
fn completion_resume_restores_review_and_completes_merged_pr() {
    if !isolated("completion_resume_restores_review_and_completes_merged_pr") {
        return;
    }
    let forge_root = tempfile::tempdir().unwrap();
    forge(forge_root.path(), false);
    let fx = Delivery::new();
    fx.promote();
    assert!(
        fx.action("pr_complete").is_err(),
        "pending delivery must fail before completion"
    );
    fx.block(&fx.run);
    fx.fail(&fx.run);
    let original_state = fx.runtime.read_run_state(&fx.run).unwrap().unwrap();
    let original_history = fx.runtime.get_task_history(&fx.task).unwrap();

    // First retry restores review, but the forge still refuses completion.
    let retry = fx.resume(&fx.run, "failed");
    assert_eq!(fx.status(), TaskStatus::Review);
    let history = fx.runtime.get_task_history(&fx.task).unwrap();
    assert_eq!(
        &history[..original_history.len()],
        original_history.as_slice()
    );
    let restoration = history.last().unwrap();
    assert_eq!(restoration.event, "resume_review_restored");
    assert_eq!(restoration.from_status, Some(TaskStatus::Blocked));
    assert_eq!(restoration.to_status, Some(TaskStatus::Review));
    assert!(restoration.note.as_ref().unwrap().contains(&retry));

    // Repeating completion-tail resume while already in review writes no
    // admission event, and never replays the implementation sentinels.
    let repeated = fx.resume(&retry, "failed");
    assert_eq!(fx.runtime.get_task_history(&fx.task).unwrap(), history);
    fx.block(&repeated);
    forge(forge_root.path(), true);
    let completed = fx.resume(&repeated, "success");
    assert_eq!(fx.status(), TaskStatus::Done);
    assert_eq!(
        fx.runtime.get_task(&fx.task).unwrap().job_run_id.as_deref(),
        Some(fx.run.as_str())
    );
    assert_eq!(
        fx.runtime.read_run_state(&fx.run).unwrap().unwrap(),
        original_state
    );
    let state = fx.runtime.read_run_state(&completed).unwrap().unwrap();
    assert_eq!(
        state.pipeline["complete_pr"]["completed_task_ids"],
        json!([fx.task])
    );
    assert_eq!(state.pipeline["complete_pr"]["merge"]["merged"], true);
    let history = fx.runtime.get_task_history(&fx.task).unwrap();
    assert!(
        history
            .last()
            .unwrap()
            .note
            .as_ref()
            .unwrap()
            .contains("merged as landed-sha")
    );
}

#[test]
fn completion_resume_cannot_invent_review_or_reverse_withdrawal() {
    if !isolated("completion_resume_cannot_invent_review_or_reverse_withdrawal") {
        return;
    }
    let forge_root = tempfile::tempdir().unwrap();
    forge(forge_root.path(), true);
    for case in [
        "early",
        "missing_review",
        "missing_checkpoint",
        "missing_completion",
        "no_authority",
        "unrelated",
        "superseding",
        "superseding_review",
        "manual_block",
        "proposed",
        "archived",
        "someday",
    ] {
        let fx = Delivery::new();
        if case != "early" && case != "missing_review" {
            fx.promote();
        }
        if case == "early" {
            fx.runtime
                .apply_task_automation_update(
                    &fx.task,
                    orbit_engine::blocked_workflow_failure_update(
                        JOB,
                        &fx.run,
                        Some("fixture_failure"),
                        Some("Implementation interrupted"),
                    ),
                )
                .unwrap();
        } else {
            fx.block(&fx.run);
        }
        fx.fail(&fx.run);
        let mut state = fx.runtime.read_run_state(&fx.run).unwrap().unwrap();
        match case {
            "early" | "missing_checkpoint" => {
                state.step_states.insert(2, JobRunState::Skipped);
                state.step_outputs.remove(&2);
                let path = fx
                    .cli
                    .home
                    .join(".orbit/resources/jobs")
                    .join(format!("{JOB}.yaml"));
                let mut job: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                job["spec"]["steps"][2]["when"] = json!("false");
                fs::write(path, job.to_string()).unwrap();
            }
            "missing_review" => {
                let output =
                    json!({"phase":"promote","performed_task_ids":[fx.task],"pr_number":"42"});
                state.record_step(2, JobRunState::Success, Some(output.clone()), None);
                state.record_pipeline_output("promote_tasks", output);
            }
            "missing_completion" => {
                // Remove completion from the catalog definition: a promotion
                // checkpoint alone is not a completion-tail retry.
                let path = fx
                    .cli
                    .home
                    .join(".orbit/resources/jobs")
                    .join(format!("{JOB}.yaml"));
                let mut job: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                job["spec"]["steps"][4]["spec"]["action"] = json!("sleep");
                job["spec"]["steps"][4]["default_input"]["seconds"] = json!(0);
                fs::write(path, job.to_string()).unwrap();
            }
            "no_authority" => {
                fx.db
                    .execute(
                        "UPDATE job_runs SET input_json=?2 WHERE run_id=?1",
                        params![
                            fx.run,
                            json!({"task_ids":[fx.task],"completion":"review","crew":"sol"})
                                .to_string()
                        ],
                    )
                    .unwrap();
            }
            "unrelated" | "superseding" | "superseding_review" => {
                let other = "jrun-superseding";
                insert_run(
                    &fx.db,
                    &fx.runtime.workspace_id().unwrap(),
                    other,
                    (case != "unrelated").then_some(fx.run.as_str()),
                    &json!({"task_ids":[fx.task]}),
                );
                fx.runtime
                    .apply_task_automation_update(
                        &fx.task,
                        TaskAutomationUpdate {
                            status: Some(TaskStatus::InProgress),
                            job_run_id: Some(other.to_string()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                if case == "superseding_review" {
                    fx.runtime
                        .apply_task_automation_update(
                            &fx.task,
                            TaskAutomationUpdate {
                                status: Some(TaskStatus::Review),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                }
                fx.block(other);
                fx.fail(other);
            }
            "manual_block" => {
                fx.runtime
                    .apply_task_automation_update(
                        &fx.task,
                        TaskAutomationUpdate {
                            status: Some(TaskStatus::InProgress),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                fx.runtime.run_tool("orbit.task.update", json!({"id":fx.task,"status":"blocked","model":"codex","note":"Operator holds this delivery."})).unwrap();
            }
            "proposed" | "archived" | "someday" => {
                if case != "archived" {
                    fx.runtime
                        .run_tool(
                            "orbit.task.update",
                            json!({"id":fx.task,"status":"backlog","model":"codex"}),
                        )
                        .unwrap();
                }
                fx.runtime.run_tool("orbit.task.update", json!({"id":fx.task,"status":case,"model":"codex","note":"Withdraw delivery."})).unwrap();
            }
            _ => {}
        }
        fx.runtime.write_run_state(&fx.run, &state).unwrap();
        let before = fx.runtime.get_task(&fx.task).unwrap();
        let expected = if case == "missing_completion" {
            "success"
        } else {
            "failed"
        };
        fx.resume(&fx.run, expected);
        let after = fx.runtime.get_task(&fx.task).unwrap();
        if case == "early" {
            assert_eq!(after.status, TaskStatus::InProgress);
        }
        assert!(
            !matches!(after.status, TaskStatus::Review | TaskStatus::Done),
            "{case} gained review authority"
        );
        if matches!(
            case,
            "unrelated" | "manual_block" | "proposed" | "archived" | "someday"
        ) {
            assert_eq!(after, before, "{case} must remain untouched");
        }
        assert!(
            fx.action("task_complete").is_err(),
            "{case}: completion guard remains authoritative"
        );
        assert!(
            !fx.runtime
                .get_task_history(&fx.task)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "resume_review_restored"),
            "{case}"
        );
    }
}

/// Note shapes written by `executor::automation::vcs::failure`. The assertion
/// is the resume outcome, not the wording.
fn failure_handoff_note(event: &str, run: &str) -> String {
    match event {
        "validation_environment_blocked" => format!(
            "required validation lacked a tool in its environment: run={run}, \
             failed_step=validate, candidate=abc, branch=orbit/candidate; the candidate was \
             not judged and no PR was opened"
        ),
        "pr_failure_handoff" => format!(
            "failure handoff published PR #7: run={run}, failed_step=validate, \
             original_base=base, target_base=agent-main, conflicts=none reported"
        ),
        "pr_conflict_blocked" => format!(
            "failure handoff published PR #7: run={run}, failed_step=sync_base, \
             original_base=base, target_base=agent-main, conflicts=src/lib.rs"
        ),
        "review_gate_escalation" => format!(
            "before-PR review gate stopped delivery: run={run}, failed_step=review_gate, \
             candidate=abc, branch=orbit/candidate; no PR was opened"
        ),
        _ => unreachable!("handoff note is only built for the four failure events"),
    }
}

fn latest_status_event(fx: &Delivery) -> orbit_types::task::TaskHistoryEntry {
    fx.runtime
        .get_task_history(&fx.task)
        .unwrap()
        .into_iter()
        .rev()
        .find(|entry| entry.to_status.is_some())
        .unwrap()
}

/// A lineage-owned failure-handoff block is resume provenance. The same event
/// naming a run outside the lineage, or a note that does not name a run, stays
/// blocked. After promotion, the handoff restores review instead of readmitting
/// implementation.
#[test]
fn resume_readmits_lineage_failure_handoff_blocks() {
    if !isolated("resume_readmits_lineage_failure_handoff_blocks") {
        return;
    }
    let forge_root = tempfile::tempdir().unwrap();
    forge(forge_root.path(), false);

    for event in [
        "validation_environment_blocked",
        "pr_failure_handoff",
        "pr_conflict_blocked",
        "review_gate_escalation",
    ] {
        let fx = Delivery::new();
        fx.skip_promotion();
        fx.block_handoff(event, &failure_handoff_note(event, &fx.run));
        fx.fail(&fx.run);
        fx.resume(&fx.run, "failed");
        assert_eq!(fx.status(), TaskStatus::InProgress, "{event}");
        let restored = latest_status_event(&fx);
        assert_eq!(restored.event, "resume_readmitted", "{event}");
        assert_eq!(restored.from_status, Some(TaskStatus::Blocked), "{event}");
        assert_eq!(restored.to_status, Some(TaskStatus::InProgress), "{event}");
    }

    let fx = Delivery::new();
    fx.skip_promotion();
    fx.block_handoff(
        "pr_failure_handoff",
        &failure_handoff_note("pr_failure_handoff", "jrun-outside-lineage"),
    );
    fx.fail(&fx.run);
    let before = fx.runtime.get_task(&fx.task).unwrap();
    fx.resume(&fx.run, "failed");
    assert_eq!(
        fx.runtime.get_task(&fx.task).unwrap(),
        before,
        "a handoff that names a run outside the lineage stays blocked"
    );

    let fx = Delivery::new();
    fx.skip_promotion();
    fx.block_handoff(
        "validation_environment_blocked",
        "required validation lacked a tool in its environment: failed_step=validate",
    );
    fx.fail(&fx.run);
    let before = fx.runtime.get_task(&fx.task).unwrap();
    fx.resume(&fx.run, "failed");
    assert_eq!(
        fx.runtime.get_task(&fx.task).unwrap(),
        before,
        "a handoff note that does not name its run stays blocked"
    );

    let fx = Delivery::new();
    fx.promote();
    fx.block_handoff(
        "pr_failure_handoff",
        &failure_handoff_note("pr_failure_handoff", &fx.run),
    );
    fx.fail(&fx.run);
    fx.resume(&fx.run, "failed");
    assert_eq!(fx.status(), TaskStatus::Review);
    let restored = latest_status_event(&fx);
    assert_eq!(restored.event, "resume_review_restored");
    assert_eq!(restored.from_status, Some(TaskStatus::Blocked));
    assert_eq!(restored.to_status, Some(TaskStatus::Review));
}
