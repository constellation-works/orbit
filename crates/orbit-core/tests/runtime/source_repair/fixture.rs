//! Real-Git host checkpoint fixture for the public source-repair scenarios.
use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, execute_deterministic_action};
use orbit_store::contracts::JobRunStepParams;
use orbit_types::task::{ExecutionLocation, ExternalRef};
use orbit_types::workflow::{JobRun, JobRunState, JobTargetType, PipelineState};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub(super) fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().into()
}

pub(super) struct Fixture {
    pub(super) _root: TempDir,
    pub(super) repo: PathBuf,
    pub(super) runtime: OrbitRuntime,
    pub(super) task: String,
    pub(super) source: JobRun,
    pub(super) state: PipelineState,
    pub(super) branch: String,
    pub(super) head: String,
    pub(super) base: String,
    pub(super) answer: PathBuf,
    _env: orbit_common::test_env::ScopedEnv,
}
impl Fixture {
    pub(super) fn new() -> Self {
        Self::with_policy("none")
    }
    pub(super) fn with_policy(timing: &str) -> Self {
        Self::build(timing, false, "17", Vec::new())
    }
    pub(super) fn publication() -> Self {
        Self::build("none", false, "4818", Vec::new())
    }
    pub(super) fn as_agent() -> Self {
        Self::build("none", true, "17", Vec::new())
    }
    pub(super) fn with_historical_pr() -> Self {
        let fixture = Self::build(
            "none",
            false,
            "4817",
            vec![ExternalRef {
                system: "github-pr".into(),
                id: "4805".into(),
                url: Some("https://github.com/owner/repository/pull/4805".into()),
            }],
        );
        // The old PR is closed; recovery must observe the authenticated current
        // publication rather than choosing a task ref by order or open status.
        std::fs::write(fixture._root.path().join("bin/gh"), format!(
            "#!/bin/sh\ncase \"$1 $2 $3\" in\n'pr view 4817') cat '{}' ;;\n'pr view 4805') echo '{{\"number\":4805,\"state\":\"CLOSED\",\"url\":\"https://github.com/owner/repository/pull/4805\"}}' ;;\n*) exit 99 ;;\nesac\n",
            fixture.answer.display()
        )).unwrap();
        fixture
    }
    fn build(timing: &str, agent: bool, number: &str, refs: Vec<ExternalRef>) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let repo = root.path().join("repo");
        let global = root.path().join("global");
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        let review = match timing {
            "before-landing" => "before_landing = true",
            "before-pr" => "before_pr = true",
            _ => "before_pr = false\nbefore_landing = false",
        };
        std::fs::write(repo.join(".orbit/config.toml"), format!("[crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\nrequired_validation_commands = [\"true\"]\n[operation]\nreview_crew = \"fixture\"\n[review]\n{review}\n")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "fixture@example.com"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("source.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let original_base = git(&repo, &["rev-parse", "HEAD"]);
        let remote = root.path().join("origin.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repository.git",
            ],
        );
        git(
            &repo,
            &[
                "config",
                &format!("url.{}.insteadOf", remote.display()),
                "https://github.com/owner/repository.git",
            ],
        );
        let jobs = global.join("resources/jobs");
        let activities = global.join("resources/activities");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(&activities).unwrap();
        for name in [
            "worktree_setup",
            "candidate_resume",
            "git_commit",
            "git_rebase",
            "pr_prepare",
            "git_push",
            "pr_open",
            "pr_complete",
            "review_gate_settle",
            "candidate_validate",
        ] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("assets/activities/{name}.yaml")),
                activities.join(format!("{name}.yaml")),
            )
            .unwrap();
        }
        let yaml = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_pr_pipeline\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - {id: worktree, target: 'activity:worktree_setup'}\n    - {id: commit, target: 'activity:git_commit'}\n    - {id: prepare_branch, target: 'activity:pr_prepare'}\n    - {id: sync_base, target: 'activity:git_rebase'}\n    - {id: push, target: 'activity:git_push'}\n    - {id: pr_open, target: 'activity:pr_open'}\n    - {id: resume_candidate, target: 'activity:candidate_resume'}\n    - {id: complete_pr, target: 'activity:pr_complete'}\n";
        std::fs::write(jobs.join("task_pr_pipeline.yaml"), yaml).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_automation_execution_location(Some(ExecutionLocation {
                machine_id: "hm_fixture".into(),
                machine_name: None,
            }));
        let task = runtime
            .add_task(TaskAddParams {
                external_refs: refs,
                title: "Published repair".into(),
                description: "Repair remaining compiler error".into(),
                acceptance_criteria: vec!["Continue the same PR".into()],
                plan: "Repair the observed failure".into(),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id;
        let branch = format!("orbit/{task}-source");
        git(&repo, &["checkout", "-qb", &branch]);
        std::fs::write(repo.join("source.txt"), "candidate needing repair\n").unwrap();
        git(&repo, &["add", "source.txt"]);
        git(&repo, &["commit", "-qm", "candidate"]);
        let committed = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("base-advance.txt"), "new base\n").unwrap();
        git(&repo, &["add", "base-advance.txt"]);
        git(&repo, &["commit", "-qm", "advance base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", &branch]);
        git(&repo, &["rebase", "main"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        assert_ne!(committed, head);
        assert!(
            !crate::git_test_support::command()
                .current_dir(&repo)
                .args(["merge-base", "--is-ancestor", &committed, &head])
                .status()
                .unwrap()
                .success(),
            "real host rewrite must have no original-commit ancestry"
        );
        git(&repo, &["push", "-q", "origin", "main", &branch]);
        let mut source = runtime
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"task_ids":[task],"completion":"done","auto_fix":true})),
                None,
            )
            .unwrap();
        source.state = JobRunState::Cancelled;
        source.finished_at = Some(Utc::now());
        let mut state = PipelineState::new(
            source.run_id.clone(),
            source.job_id.clone(),
            source.input.clone().unwrap(),
        );
        for (index, id, output) in [
            (
                0,
                "worktree",
                json!({"head_ref":branch,"base_sha":original_base,"job_run_id":source.run_id}),
            ),
            (
                1,
                "commit",
                json!({"phase":"commit","decision":"performed","committed":true,"task_id":task,"job_run_id":source.run_id,"base_sha":original_base,"commit_sha":committed}),
            ),
            (
                2,
                "prepare_branch",
                json!({"phase":"prepare","base":"main","base_ref":"origin/main","base_sha":base,"head":branch,"head_sha":committed,"remote_sha":null,"sync_required":true}),
            ),
            (
                3,
                "sync_base",
                json!({"phase":"rebase","base":"main","base_ref":"origin/main","base_sha":base,"head":branch,"head_sha_before":committed,"head_sha":head,"rewritten":true,"remote_sha_before":null}),
            ),
            (
                4,
                "push",
                json!({"phase":"push","local_sha":head,"branch":branch,"remote_sha_before":null}),
            ),
            (
                5,
                "pr_open",
                json!({"phase":"pr_open","pr_number":number,"head":branch,"base":"main","base_ref":"origin/main","base_sha":base,"pr_url":format!("https://github.com/owner/repository/pull/{number}")}),
            ),
        ] {
            state.record_step(index, JobRunState::Success, Some(output.clone()), None);
            state.record_pipeline_output(id, output);
            runtime
                .complete_job_run_step(
                    &source.run_id,
                    &JobRunStepParams {
                        step_index: index as usize,
                        target_type: JobTargetType::Activity,
                        target_id: id.into(),
                        state: JobRunState::Success,
                        started_at: source.created_at,
                        finished_at: Utc::now(),
                        duration_ms: Some(1),
                        exit_code: Some(0),
                        lock_wait_ms: None,
                        agent_response_json: None,
                        error_code: None,
                        error_message: None,
                    },
                )
                .unwrap();
        }
        state.compact_for_terminal(JobRunState::Cancelled);
        runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &source, None)
            .unwrap();
        runtime.write_run_state(&source.run_id, &state).unwrap();
        runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Review),
                    job_run_id: Some(source.run_id.clone()),
                    external_refs: vec![ExternalRef {
                        system: "github-pr".into(),
                        id: number.into(),
                        url: Some(format!("https://github.com/owner/repository/pull/{number}")),
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        let answer = root.path().join("pr.json");
        std::fs::write(&answer,json!({"number":number.parse::<u64>().unwrap(),"state":"OPEN","headRefName":branch,"headRefOid":head,"baseRefName":"main","url":format!("https://github.com/owner/repository/pull/{number}"),"autoMergeRequest":null,"mergedAt":null,"mergeCommit":null,"mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED","statusCheckRollup":[],"reviewDecision":"","isDraft":true}).to_string()).unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n[ \"$1 $2 $3\" = \"pr view {number}\" ] || exit 99\ncat '{}'\n",
                root.path().join("gh-calls").display(),
                answer.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let env = orbit_common::test_env::scoped([
            ("PATH", Some(path.to_str().unwrap())),
            ("ORBIT_OPERATOR", Some("1")),
            ("ORBIT_AGENT_MODEL", agent.then_some("codex")),
        ]);
        orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
        Self {
            _root: root,
            repo,
            runtime,
            task,
            source,
            state,
            branch,
            head,
            base,
            answer,
            _env: env,
        }
    }
    /// Replace only isolated fixture evidence, including deliberately inconsistent refs.
    pub(super) fn replace_refs(&self, refs: Vec<ExternalRef>) {
        use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
        let registry =
            TaskRegistryStore::open(&task_registry_path(&self.runtime.global_root())).unwrap();
        orbit_store::compose::workspace_coordinated_backends(
            registry,
            self.runtime.workspace_id().unwrap(),
            self.runtime.sqlite_store().unwrap().clone(),
        )
        .unwrap()
        .task
        .document
        .update_task_document(
            &self.task,
            orbit_store::contracts::TaskDocumentUpdateParams {
                actor: "fixture".into(),
                external_refs: Some(refs),
                ..Default::default()
            },
        )
        .unwrap();
    }
    /// Model the durable host checkpoints after a reviewer correction was published.
    pub(super) fn publish_review(&mut self, landing: bool) {
        let implementation = self.head.clone();
        std::fs::write(self.repo.join("review.txt"), "reviewer correction\n").unwrap();
        git(&self.repo, &["add", "review.txt"]);
        git(&self.repo, &["commit", "-qm", "review: correct candidate"]);
        self.head = git(&self.repo, &["rev-parse", "HEAD"]);
        git(&self.repo, &["push", "-q", "origin", &self.branch]);
        let mut answer: Value =
            serde_json::from_slice(&std::fs::read(&self.answer).unwrap()).unwrap();
        answer["headRefOid"] = json!(self.head);
        std::fs::write(&self.answer, answer.to_string()).unwrap();
        let settle = if landing {
            "landing_review_gate_settle"
        } else {
            "review_gate_settle"
        };
        let validate = if landing {
            "landing_review_validate"
        } else {
            "review_validate"
        };
        self.state.pipeline[settle] = json!({"gate":"passed","reviewer_fixed":true,
            "implementation_head_sha":implementation,"reviewed_head_sha":self.head,"reviewed_base_sha":self.base});
        self.state.pipeline[validate] = json!({"phase":"validate","decision":"passed",
            "branch":self.branch,"tested_head":self.head});
        if landing {
            self.state.pipeline["landing_push"] = json!({"phase":"push","branch":self.branch,
                "local_sha":self.head,"remote_sha_before":implementation});
        } else {
            self.state.pipeline["push"]["local_sha"] = json!(self.head);
        }
        let path = self
            ._root
            .path()
            .join("global/resources/jobs/task_pr_pipeline.yaml");
        let mut job: Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let steps = job["spec"]["steps"].as_array_mut().unwrap();
        let offset = if landing { 6 } else { 4 };
        steps.insert(
            offset,
            json!({"id":settle,"target":"activity:review_gate_settle"}),
        );
        steps.insert(offset + 1, json!({"id":validate,"target":"activity:candidate_validate",
            "default_input":{"ownership_base_sha":format!("{{{{ steps.{settle}.output.implementation_head_sha }}}}")}}));
        if landing {
            steps.insert(
                offset + 2,
                json!({"id":"landing_push","target":"activity:git_push",
                "default_input":{"lease_remote_sha":"{{ steps.push.output.local_sha }}",
                    "pr_number":"{{ steps.pr_open.output.pr_number }}"}}),
            );
        }
        self.state.step_states.clear();
        self.state.step_outputs.clear();
        self.state.step_output_pointers.clear();
        for (index, step) in steps.iter().enumerate() {
            let id = step["id"].as_str().unwrap();
            if let Some(output) = self.state.pipeline.get(id).cloned() {
                self.state
                    .record_step(index as u32, JobRunState::Success, Some(output), None);
                self.runtime
                    .complete_job_run_step(
                        &self.source.run_id,
                        &JobRunStepParams {
                            step_index: index,
                            target_type: JobTargetType::Activity,
                            target_id: id.into(),
                            state: JobRunState::Success,
                            started_at: self.source.created_at,
                            finished_at: Utc::now(),
                            duration_ms: Some(1),
                            exit_code: Some(0),
                            lock_wait_ms: None,
                            agent_response_json: None,
                            error_code: None,
                            error_message: None,
                        },
                    )
                    .unwrap();
            }
        }
        self.state.compact_for_terminal(JobRunState::Cancelled);
        std::fs::write(path, serde_yaml::to_string(&job).unwrap()).unwrap();
        self.runtime
            .write_run_state(&self.source.run_id, &self.state)
            .unwrap();
    }
    pub(super) fn request(&self, action: &str) -> Value {
        json!({"id":self.source.run_id,"source_repair":{"action":action,"task_id":self.task}})
    }
    pub(super) fn inspect(&self) -> Value {
        self.runtime
            .run_tool_as_human("orbit.workflow.run.resume", self.request("inspect"))
            .unwrap()["binding"]
            .clone()
    }
    pub(super) fn submit(&self, binding: Value) -> Value {
        let mut input = self.request("submit");
        input["source_repair"]["binding"] = binding;
        input["source_repair"]["request_key"] = json!("repair-1");
        input["source_repair"]["diagnosis"] = json!(
            "CI run 42/job 7: compiler error E0603 on the published head. Repair its visibility without replacing this PR."
        );
        input
    }
    pub(super) fn wait(&self, id: &str) {
        self.runtime
            .wait_pipeline_runs(&[id.into()], 10, 1, Some("test"))
            .unwrap();
    }
    /// The substitute worker has exited and been joined. The harness now
    /// drives this admitted run's host actions in-process as its worker.
    pub(super) fn start_actions(&self, id: &str) {
        let mut run = self.runtime.show_job_run_observed(id).unwrap();
        run.state = JobRunState::Running;
        run.started_at = Some(Utc::now());
        run.finished_at = None;
        run.pid = Some(std::process::id());
        self.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&self.runtime.workspace_id().unwrap(), &run, None)
            .unwrap();
    }
    pub(super) fn action(&self, name: &str, input: Value) -> Value {
        if name.starts_with("review_gate_") {
            self.runtime
                .run_deterministic(name, &json!({}), &input, Default::default())
                .unwrap()
        } else {
            execute_deterministic_action(
                &self.runtime,
                name,
                &json!({}),
                &input,
                false,
                &HashMap::new(),
                None,
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"))
        }
    }
    pub(super) fn assert_original(&self) {
        assert_eq!(
            self.runtime
                .show_job_run_observed(&self.source.run_id)
                .unwrap()
                .state,
            JobRunState::Cancelled
        );
        assert_eq!(
            self.runtime
                .read_run_state(&self.source.run_id)
                .unwrap()
                .unwrap(),
            self.state
        );
    }
}
