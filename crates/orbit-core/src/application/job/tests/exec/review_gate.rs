//! The real `task_pr_pipeline` with the before-PR review gate [ORB-11333].
//!
//! Git mechanics that reach a remote are scripted; the review gate actions,
//! the failure handoff, and the task/run records are real. The reviewer
//! agent is replaced by a deterministic stub that persists the same report
//! artifact the reviewer tool would. Epic-pipeline coverage lives in the
//! sibling `epic_review_gate` module.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_engine::{DispatchError, ResolvedCliExecutor, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{
    FindingDisposition, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    ReviewFinding, ReviewReport, ReviewValidation, ReviewVerdict, ValidationOutcome,
    ValidationRole,
};
use serde_json::{Value, json};

use super::{
    git_in, resolved_job, retarget_engine_actions_for_scripted_host, seed_default_catalogs,
    test_runtime_with_workspace_config, try_execute_job,
};
use crate::OrbitRuntime;
use crate::application::review::install_review_admission;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

pub(super) const GATED_CONFIG: &str = r#"
[crews.implementer]
model = "impl-model"
provider = "codex"
backend = "cli"

[crews.reviewers]
model = "review-model"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "implementer"

[operation]
review_policy = "before-pr"
review_crew = "reviewers"
"#;

pub(super) fn stub_agent_activity(global_root: &Path, name: &str, action: &str) {
    std::fs::write(
        global_root.join(format!("resources/activities/{name}.yaml")),
        format!(
            "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: {name}\nspec:\n  type: deterministic\n  description: Test stub.\n  input_schema_json:\n    type: object\n  output_schema_json:\n    type: object\n  action: {action}\n  config: {{}}\n"
        ),
    )
    .expect("stub agent activity");
}

struct Pipeline {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    task_id: String,
    run_id: String,
    base_sha: String,
}

fn pipeline(config: &str) -> Pipeline {
    let (root, runtime, repo, global_root) = test_runtime_with_workspace_config(config);
    seed_default_catalogs(&global_root);
    stub_agent_activity(&global_root, "agent_implement", "scripted_agent_implement");
    stub_agent_activity(&global_root, "agent_review_repair", "scripted_review");

    let remote = root.path().join("remote.git");
    git_in(
        root.path(),
        &["init", "--bare", remote.to_str().expect("remote path")],
    );
    git_in(&repo, &["init"]);
    git_in(&repo, &["checkout", "-b", "main"]);
    git_in(&repo, &["config", "user.name", "Orbit Test"]);
    git_in(
        &repo,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("ignore");
    std::fs::write(repo.join("src.txt"), "base\n").expect("write");
    git_in(&repo, &["add", ".gitignore", "src.txt"]);
    git_in(&repo, &["commit", "-m", "base"]);
    git_in(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git_in(&repo, &["push", "-u", "origin", "main"]);
    let base_sha = git_stdout(&repo, &["rev-parse", "HEAD"]);

    let task = runtime
        .add_task(TaskAddParams {
            title: "Gated pipeline task".to_string(),
            description: "Fixture task delivered through the gated PR pipeline.".to_string(),
            acceptance_criteria: vec!["src.txt says implemented.".to_string()],
            plan: "Edit src.txt.".to_string(),
            context_files: vec!["file:src.txt".to_string()],
            workspace_path: Some(".".to_string()),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::InProgress),
            ..TaskAddParams::default()
        })
        .expect("seed task");
    git_in(&repo, &["checkout", "-b", &format!("orbit/{}", task.id)]);
    std::fs::write(repo.join("src.txt"), "base\nimplemented\n").expect("implement");
    git_in(&repo, &["add", "src.txt"]);
    git_in(
        &repo,
        &["commit", "-m", &format!("feat: implement [{}]", task.id)],
    );

    let mut input = json!({
        "task_ids": [task.id],
        "base_branch": "main",
        "base_sync": "local",
        "allowed_crews": [],
    });
    install_review_admission(&runtime, "task_pr_pipeline", &mut input, None, false)
        .expect("capture admission");
    let run = RuntimeHost::insert_job_run(
        &runtime,
        "task_pr_pipeline",
        1,
        Utc::now(),
        Some(input),
        None,
    )
    .expect("insert run");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                job_run_id: Some(Some(run.run_id.clone())),
                execution_summary: Some(
                    "Outcome: success\n\nChanges:\n- src.txt says implemented.".to_string(),
                ),
                ..TaskUpdateParams::default()
            },
        )
        .expect("bind task");

    Pipeline {
        _root: root,
        runtime,
        repo,
        task_id: task.id,
        run_id: run.run_id,
        base_sha,
    }
}

pub(super) fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

impl Pipeline {
    fn run_input(&self) -> Value {
        self.runtime
            .get_job_run_backend(&self.run_id)
            .expect("run")
            .expect("present")
            .input
            .expect("input")
    }

    fn execute(&self, host: &ScriptedReviewHost<'_>) -> Result<(), DispatchError> {
        let mut job = resolved_job(&self.runtime, "task_pr_pipeline");
        retarget_engine_actions_for_scripted_host(&mut job);
        try_execute_job(
            &self.runtime,
            &self.repo,
            host,
            job,
            self.run_input(),
            &self.run_id,
        )
        .map(|_| ())
    }

    fn head(&self) -> String {
        git_stdout(&self.repo, &["rev-parse", "HEAD"])
    }
}

/// What the stubbed reviewer does when the pipeline reaches it.
#[derive(Clone, Copy)]
pub(super) struct ReviewerScript {
    pub(super) verdict: ReviewVerdict,
    pub(super) repair: bool,
}

struct ScriptedReviewHost<'a> {
    pipeline: &'a Pipeline,
    reviewer: ReviewerScript,
    calls: Mutex<Vec<String>>,
    pr_open_inputs: Mutex<Vec<Value>>,
    private_ops: Mutex<Vec<String>>,
}

impl<'a> ScriptedReviewHost<'a> {
    fn new(pipeline: &'a Pipeline, reviewer: ReviewerScript) -> Self {
        Self {
            pipeline,
            reviewer,
            calls: Mutex::new(Vec::new()),
            pr_open_inputs: Mutex::new(Vec::new()),
            private_ops: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    fn pr_open_inputs(&self) -> Vec<Value> {
        self.pr_open_inputs.lock().expect("pr open inputs").clone()
    }

    fn private_ops(&self) -> Vec<String> {
        self.private_ops.lock().expect("private ops").clone()
    }

    fn write_report(&self, attempt_id: &str) {
        let repaired = self.reviewer.repair;
        let report = ReviewReport {
            schema_version: REVIEW_CONTRACT_VERSION,
            attempt_id: attempt_id.to_string(),
            verdict: self.reviewer.verdict,
            summary: "Scripted review.".to_string(),
            findings: if repaired || self.reviewer.verdict == ReviewVerdict::ChangesRequired {
                vec![ReviewFinding {
                    id: "F1".to_string(),
                    severity: "low".to_string(),
                    summary: "trailing note".to_string(),
                    paths: vec!["src.txt".to_string()],
                    disposition: if repaired {
                        FindingDisposition::Repaired
                    } else {
                        FindingDisposition::Open
                    },
                }]
            } else {
                Vec::new()
            },
            validation: vec![ReviewValidation {
                command: "make ci-fast".to_string(),
                outcome: ValidationOutcome::Passed,
                role: ValidationRole::Required,
                note: None,
                check: None,
            }],
            escalation: (self.reviewer.verdict == ReviewVerdict::ChangesRequired)
                .then(|| "decide on the note".to_string()),
        };
        if repaired {
            std::fs::write(
                self.pipeline.repo.join("src.txt"),
                "base\nimplemented\nrepaired\n",
            )
            .expect("repair");
        }
        self.pipeline
            .runtime
            .update_task(
                &self.pipeline.task_id,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact {
                        path: REVIEW_REPORT_ARTIFACT.to_string(),
                        content: serde_json::to_vec(&report).expect("report"),
                        media_type: "application/json".to_string(),
                        created_by: None,
                    }],
                    ..TaskUpdateParams::default()
                },
            )
            .expect("persist report");
    }
}

impl RuntimeHost for ScriptedReviewHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls.lock().expect("calls").push(action.to_string());
        let repo = &self.pipeline.repo;
        let branch = format!("orbit/{}", self.pipeline.task_id);
        let head = self.pipeline.head();
        match action {
            "scripted_worktree_setup" => Ok(json!({
                "workspace_path": repo,
                "job_run_id": self.pipeline.run_id,
                "base_ref": "main",
                "base_sha": self.pipeline.base_sha,
            })),
            "scripted_agent_implement" => Ok(json!({ "summary": "implemented" })),
            "scripted_git_commit" => Ok(json!({
                "phase": "commit",
                "decision": "already_committed",
                "committed": false,
                "skipped_no_diff_expected": false,
            })),
            "scripted_pr_prepare" => Ok(json!({
                "phase": "prepare",
                "decision": "already_fresh",
                "head": branch,
                "head_sha": head,
                "base": "main",
                "base_ref": "main",
                "base_sha": self.pipeline.base_sha,
                "remote_sha": Value::Null,
                "commits_behind": 0,
                "commits_ahead": 1,
                "sync_required": false,
            })),
            "scripted_git_rebase" => Ok(json!({
                "phase": "rebase",
                "decision": "skipped_current",
                "head": branch,
                "head_sha": head,
                "head_sha_before": head,
                "base": "main",
                "base_ref": "main",
                "base_sha": self.pipeline.base_sha,
                "remote_sha_before": Value::Null,
                "rewritten": false,
            })),
            "scripted_review" => {
                let attempt_id = input
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .expect("reviewer receives the attempt id");
                assert_eq!(
                    input.get("crew").and_then(Value::as_str),
                    Some("reviewers"),
                    "the reviewer runs as the configured review crew"
                );
                self.write_report(attempt_id);
                Ok(json!({ "verdict": self.reviewer.verdict.as_str() }))
            }
            "scripted_git_push" => Ok(json!({
                "phase": "push",
                "decision": "performed",
                "branch": branch,
                "local_sha": head,
                "force_with_lease": false,
            })),
            "scripted_pr_open" => {
                self.pr_open_inputs
                    .lock()
                    .expect("pr open inputs")
                    .push(input.clone());
                Ok(json!({
                    "phase": "pr_open",
                    "decision": "performed",
                    "pr_created": true,
                    "pr_reused": false,
                    "pr_number": "42",
                    "pr_url": "https://example.test/pr/42",
                    "base": "main",
                    "head": branch,
                }))
            }
            "scripted_pr_promote" => Ok(json!({
                "phase": "promote",
                "decision": "performed",
                "performed_task_ids": [self.pipeline.task_id],
                "reused_task_ids": [],
                "pr_number": "42",
            })),
            _ => <OrbitRuntime as RuntimeHost>::run_deterministic(
                &self.pipeline.runtime,
                action,
                config,
                input,
                tool_context,
            ),
        }
    }

    fn has_deterministic_action(&self, action: &str) -> bool {
        action.starts_with("scripted_")
            || <OrbitRuntime as RuntimeHost>::has_deterministic_action(
                &self.pipeline.runtime,
                action,
            )
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        _input: Value,
    ) -> Result<Value, orbit_common::OrbitError> {
        self.private_ops
            .lock()
            .expect("private ops")
            .push(operation.to_string());
        match operation {
            "push" => Ok(json!({ "stdout": "", "stderr": "" })),
            other => Err(orbit_common::OrbitError::Execution(format!(
                "unexpected private VCS operation {other}"
            ))),
        }
    }

    fn get_task(&self, task_id: &str) -> Result<Task, orbit_common::OrbitError> {
        self.pipeline.runtime.get_task(task_id)
    }

    fn get_task_comments(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::task::TaskComment>, orbit_common::OrbitError> {
        self.pipeline.runtime.get_task_comments(task_id)
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, orbit_common::OrbitError> {
        RuntimeHost::list_tasks_filtered(
            &self.pipeline.runtime,
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: orbit_engine::TaskAutomationUpdate,
    ) -> Result<(), orbit_common::OrbitError> {
        RuntimeHost::apply_task_automation_update(&self.pipeline.runtime, task_id, update)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: orbit_engine::TaskActivityUpdate,
    ) -> Result<Task, orbit_common::OrbitError> {
        self.pipeline
            .runtime
            .update_task_from_activity(task_id, update)
    }

    fn resolved_crew_model(
        &self,
        run_id: &str,
    ) -> Result<Option<String>, orbit_common::OrbitError> {
        RuntimeHost::resolved_crew_model(&self.pipeline.runtime, run_id)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        <OrbitRuntime as RuntimeHost>::resolve_cli_executor(&self.pipeline.runtime, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            &self.pipeline.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

pub(super) fn positions(calls: &[String], names: &[&str]) -> Vec<usize> {
    names
        .iter()
        .map(|name| {
            calls
                .iter()
                .position(|call| call == name)
                .unwrap_or_else(|| panic!("{name} was not called: {calls:?}"))
        })
        .collect()
}

#[test]
fn before_pr_pass_with_repairs_publishes_only_the_settled_candidate() {
    let pipeline = pipeline(GATED_CONFIG);
    let implementation = pipeline.head();
    let host = ScriptedReviewHost::new(
        &pipeline,
        ReviewerScript {
            verdict: ReviewVerdict::PassedWithRepairs,
            repair: true,
        },
    );

    pipeline.execute(&host).expect("gated pipeline succeeds");

    let calls = host.calls();
    let order = positions(
        &calls,
        &[
            "scripted_git_rebase",
            "review_gate_admit",
            "scripted_review",
            "review_gate_settle",
            "scripted_git_push",
            "scripted_pr_open",
            "scripted_pr_promote",
        ],
    );
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "the gate runs after base sync and before push/PR: {calls:?}"
    );

    let final_head = pipeline.head();
    assert_ne!(
        final_head, implementation,
        "the repair is a separate commit"
    );
    assert_eq!(
        git_stdout(&pipeline.repo, &["log", "-1", "--format=%an", "HEAD"]),
        "codex-reviewer"
    );
    let opened = host.pr_open_inputs();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["reviewed_head_sha"], final_head);
    assert_eq!(opened[0]["reviewed_base_sha"], pipeline.base_sha);
    assert!(
        pipeline
            .runtime
            .get_task_artifact(&pipeline.task_id, REVIEW_GATE_ARTIFACT)
            .expect("read")
            .is_some()
    );
    let review = crate::application::review::task_review_projection(
        &pipeline.runtime,
        &pipeline.runtime.get_task(&pipeline.task_id).expect("task"),
        &pipeline
            .runtime
            .get_task_artifact_manifest(&pipeline.task_id)
            .expect("manifest"),
    )
    .expect("projection")
    .expect("present");
    assert_eq!(review["verdict"], "passed_with_repairs");
    assert_eq!(
        review["assurance"],
        "independent_review_with_self_authored_repairs"
    );
    assert_eq!(review["consumed"]["repair_cycles"], 1);
}

#[test]
fn before_pr_changes_required_blocks_the_task_without_opening_a_pr() {
    let pipeline = pipeline(GATED_CONFIG);
    let host = ScriptedReviewHost::new(
        &pipeline,
        ReviewerScript {
            verdict: ReviewVerdict::ChangesRequired,
            repair: false,
        },
    );

    let error = pipeline
        .execute(&host)
        .expect_err("the gate stops delivery");
    assert!(error.to_string().contains("review_gate_blocked"), "{error}");

    let calls = host.calls();
    assert!(calls.iter().any(|call| call == "review_gate_settle"));
    assert!(
        !calls.iter().any(|call| call == "scripted_pr_open"),
        "no PR is opened for a blocked candidate: {calls:?}"
    );
    assert_eq!(
        host.private_ops(),
        vec!["push"],
        "the failure handoff pushes the candidate but creates no PR"
    );
    let task = pipeline.runtime.get_task(&pipeline.task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Blocked);
    assert!(
        task.external_refs
            .iter()
            .all(|reference| reference.system != "github-pr"),
        "no PR reference is attached"
    );
    let comments = pipeline
        .runtime
        .get_task_comments(&pipeline.task_id)
        .expect("comments");
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains("Review gate escalation")),
        "the escalation is recorded on the task"
    );
    let review = crate::application::review::task_review_projection(
        &pipeline.runtime,
        &task,
        &pipeline
            .runtime
            .get_task_artifact_manifest(&pipeline.task_id)
            .expect("manifest"),
    )
    .expect("projection")
    .expect("present");
    assert_eq!(review["verdict"], "changes_required");
    assert_eq!(review["passed"], false);
}

#[test]
fn none_and_after_landing_policies_never_start_a_reviewer() {
    for policy in ["none", "after-landing"] {
        let config = format!(
            "[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"{policy}\"\n"
        );
        let pipeline = pipeline(&config);
        let host = ScriptedReviewHost::new(
            &pipeline,
            ReviewerScript {
                verdict: ReviewVerdict::PassedWithoutRepairs,
                repair: false,
            },
        );

        pipeline.execute(&host).expect("ungated pipeline succeeds");
        let calls = host.calls();
        assert!(calls.iter().any(|call| call == "review_gate_admit"));
        assert!(
            !calls.iter().any(|call| call == "scripted_review"),
            "{policy}: no reviewer starts: {calls:?}"
        );
        let opened = host.pr_open_inputs();
        assert_eq!(opened.len(), 1);
        assert_eq!(
            opened[0]["reviewed_head_sha"], "",
            "{policy}: no pinned gate"
        );
        assert!(
            pipeline
                .runtime
                .get_task_artifact(&pipeline.task_id, REVIEW_GATE_ARTIFACT)
                .expect("read")
                .is_none(),
            "{policy}: no certificate is issued"
        );
    }
}
