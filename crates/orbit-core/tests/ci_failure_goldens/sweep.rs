//! Whole CI-failure sweeps: host-owned collection through a substitute `gh`,
//! then filing, against a workspace whose `origin` is a local bare remote.
//!
//! `PATH` is process-global, so each test body re-runs in an isolated child of
//! this binary with the substitute first on `PATH`. The substitute answers
//! from files the child writes under `ORBIT_FAKE_GH_STATE`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};
use tempfile::TempDir;

const CHILD_ENV: &str = "ORBIT_TEST_CI_SWEEP_CHILD";
const STATE_ENV: &str = "ORBIT_FAKE_GH_STATE";

const FAKE_GH: &str = r#"#!/bin/sh
set -eu
state=${ORBIT_FAKE_GH_STATE:?}
printf '%s\n' "$*" >> "$state/calls"
case "$1 ${2:-}" in
  "auth status") exit 0 ;;
  "repo view") cat "$state/repo.json" ;;
  "pr list") printf '[]\n' ;;
  "run list") cat "$state/runs.json" ;;
  "run view")
    case "$*" in
      *--log*) cat "$state/run-$3.log" ;;
      *) cat "$state/run-$3.json" ;;
    esac
    ;;
  "api --method") printf '{"jobs":[]}\n' ;;
  *)
    echo "fake gh: unsupported call: $*" >&2
    exit 2
    ;;
esac
"#;

pub(crate) const JOB_NAME: &str = "Check / Clippy / Test";
pub(crate) const FAILED_STEP: &str = "Test";

/// Run `test` in a child whose `PATH` starts with the substitute `gh`.
/// Returns true inside that child.
pub(crate) fn isolated_with_fake_gh(test: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os(CHILD_ENV).is_some() {
        return true;
    }
    let sandbox = TempDir::new().unwrap();
    let bin = sandbox.path().join("bin");
    let state = sandbox.path().join("gh-state");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("GIT_") || name.starts_with("GH_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env(STATE_ENV, &state)
        .env("PATH", std::env::join_paths(path).unwrap())
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .current_dir(sandbox.path());
    let logs = TempDir::new().unwrap();
    let output = orbit_common::test_env::run_child_test(&mut command, test, logs.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

/// A workspace whose `origin` holds `agent-main` and `main` at one commit.
pub(crate) struct Workspace {
    _root: TempDir,
    pub(crate) repo: PathBuf,
    pub(crate) head: String,
    pub(crate) runtime: OrbitRuntime,
    state: PathBuf,
    runs: Vec<Value>,
}

impl Workspace {
    pub(crate) fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let origin = root.path().join("origin.git");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::write(repo.join(".orbit/config.toml"), "").unwrap();
        let head = super::commit_chain(&repo, 1).remove(0);
        git(&origin, &["init", "-q", "--bare"]);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(
            &repo,
            &[
                "push",
                "-q",
                "origin",
                "HEAD:refs/heads/agent-main",
                "HEAD:refs/heads/main",
            ],
        );
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let state = PathBuf::from(std::env::var_os(STATE_ENV).expect("fake gh state"));
        write_json(
            &state.join("repo.json"),
            &json!({"name": "orbit", "nameWithOwner": "acme/orbit",
                    "defaultBranchRef": {"name": "main"}}),
        );
        Self {
            _root: root,
            repo,
            head,
            runtime,
            state,
            runs: Vec::new(),
        }
    }

    /// Publish one more completed red push run of `CI` on agent-main whose
    /// single failed job serves `log` for every log read.
    pub(crate) fn push_red_run(&mut self, run_id: u64, job_id: u64, log: &str) {
        let head = self.head.clone();
        self.publish_red_run("agent-main", "push", &head, run_id, &[job_id], log);
    }

    /// Publish a completed red `CI` run, newer than every earlier one, whose
    /// failed jobs each serve `log` for every log read.
    fn publish_red_run(
        &mut self,
        branch: &str,
        event: &str,
        head_sha: &str,
        run_id: u64,
        job_ids: &[u64],
        log: &str,
    ) {
        let created_at = format!("2026-10-10T06:{:02}:00Z", self.runs.len() % 60);
        let url = format!("https://github.com/acme/orbit/actions/runs/{run_id}");
        let run = json!({
            "databaseId": run_id, "number": run_id, "workflowName": "CI",
            "displayTitle": format!("CI on {branch}"), "status": "completed",
            "conclusion": "failure", "event": event, "headBranch": branch,
            "headSha": head_sha, "createdAt": created_at, "startedAt": created_at,
            "updatedAt": created_at, "url": url,
        });
        let mut view = run.clone();
        view["jobs"] = job_ids
            .iter()
            .enumerate()
            .map(|(index, job_id)| {
                let name = match index {
                    0 => JOB_NAME.to_string(),
                    _ => format!("{JOB_NAME} {index}"),
                };
                json!({
                    "databaseId": job_id, "name": name,
                    "status": "completed", "conclusion": "failure",
                    "startedAt": created_at, "completedAt": created_at,
                    "steps": [
                        {"number": 1, "name": "Set up job", "status": "completed", "conclusion": "success"},
                        {"number": 2, "name": "Run actions/checkout@v4", "status": "completed", "conclusion": "success"},
                        {"number": 3, "name": FAILED_STEP, "status": "completed", "conclusion": "failure"},
                    ],
                })
            })
            .collect();
        write_json(&self.state.join(format!("run-{run_id}.json")), &view);
        std::fs::write(
            self.state.join(format!("run-{run_id}.log")),
            log.replace("{checkout}", &self.head),
        )
        .unwrap();
        self.runs.insert(0, run);
        write_json(&self.state.join("runs.json"), &json!(self.runs));
    }

    /// One scheduled sweep: a fresh job run identity, collection, then filing.
    pub(crate) fn sweep(&self) -> (Value, Result<Value, String>) {
        let job_run = self
            .runtime
            .insert_job_run(
                "ci_failure_sweep_pipeline",
                1,
                chrono::Utc::now(),
                None,
                None,
            )
            .unwrap();
        let collected = orbit_engine::execute_deterministic_action(
            &self.runtime,
            "collect_ci_evidence",
            &json!({}),
            &json!({
                "workspace_path": self.repo,
                "integration_branch": "agent-main",
                "max_investigated_runs": 6,
                "investigation_cursor": 0,
                "job_run_id": job_run.run_id,
            }),
            false,
            &HashMap::new(),
            None,
        )
        .expect("collect CI evidence");
        let evidence = collected["ci_evidence"].clone();
        let filed = self
            .runtime
            .run_deterministic(
                "file_ci_failure_tasks",
                &json!({}),
                &json!({"ci_evidence": evidence}),
                ToolContext::default(),
            )
            .map_err(|error| error.to_string());
        (evidence, filed)
    }
}

fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(dir).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// One gh display line for the failing job, which gh could not attribute to
/// a step.
pub(crate) fn unattributed(payload: &str) -> String {
    format!("{JOB_NAME}\tUNKNOWN STEP\t2026-10-10T06:37:01.0000000Z {payload}\n")
}

/// A job log no diagnostic can be bound to: two commands exit nonzero, so
/// neither is the job's unique failure, and the display is truncated.
fn unbindable_log() -> String {
    let mut log = unattributed("HEAD is now at {checkout} Merge into agent-main");
    for (command, code) in [
        ("cargo clippy --workspace", 101),
        ("cargo test --workspace", 101),
    ] {
        log.push_str(&unattributed(&format!("##[group]Run {command}")));
        log.push_str(&unattributed("##[endgroup]"));
        log.push_str(&unattributed("error: could not compile `orbit-core`").repeat(200));
        log.push_str(&unattributed(&format!(
            "##[error]Process completed with exit code {code}."
        )));
    }
    log
}

fn open_sweep_tasks(runtime: &OrbitRuntime) -> Vec<orbit_types::task::Task> {
    runtime
        .list_tasks()
        .unwrap()
        .into_iter()
        .filter(|task| task.tags.iter().any(|tag| tag == "ci-failure-sweep"))
        .filter(|task| task.status == orbit_types::task::TaskStatus::Proposed)
        .collect()
}

/// B4/W4: agent-main stays red on a new push every sweep and no diagnostic
/// can be bound. Gaps keyed by run never repeat, so before escalation every
/// sweep failed retryable and nothing was ever filed.
#[test]
fn ci_agent_main_red_for_three_sweeps_always_has_an_open_task() {
    if !isolated_with_fake_gh("sweep::ci_agent_main_red_for_three_sweeps_always_has_an_open_task") {
        return;
    }
    let mut workspace = Workspace::new();
    for sweep in 1..=2u64 {
        workspace.push_red_run(8_000 + sweep, 9_100 + sweep, &unbindable_log());
        let (evidence, filed) = workspace.sweep();
        assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
        let error = filed.expect_err("an unfileable finding still retries this sweep");
        assert!(error.contains("job_log_truncated"), "{error}");
        assert!(open_sweep_tasks(&workspace.runtime).is_empty());
    }

    workspace.push_red_run(8_003, 9_103, &unbindable_log());
    let (evidence, filed) = workspace.sweep();
    let output = filed.expect("the third sweep escalates instead of retrying");
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert_eq!(
        evidence["summary"]["landing_evidence_incomplete_run_ids"],
        json!([8_003])
    );
    assert_eq!(output["outcome"], json!("current_failures"));
    assert_eq!(output["evidence_incomplete"][0]["outcome"], json!("filed"));
    let tasks = open_sweep_tasks(&workspace.runtime);
    assert_eq!(tasks.len(), 1, "{output:#}");
    let task = &tasks[0];
    assert_eq!(task.id, output["evidence_incomplete"][0]["task_id"]);
    assert!(
        task.description
            .contains("https://github.com/acme/orbit/actions/runs/8003"),
        "the escalated task links the failing run: {}",
        task.description
    );
    assert!(
        task.description.contains("job_log_truncated"),
        "the escalated task names its evidence gap: {}",
        task.description
    );

    workspace.push_red_run(8_004, 9_104, &unbindable_log());
    let (_, filed) = workspace.sweep();
    let output = filed.expect("a later sweep keeps the escalation");
    assert_eq!(
        output["evidence_incomplete"][0]["outcome"],
        json!("existing")
    );
    assert_eq!(output["evidence_incomplete"][0]["task_id"], json!(task.id));
    assert_eq!(open_sweep_tasks(&workspace.runtime).len(), 1);
}

/// B4: newer red runs on task branches origin has since deleted outnumber the
/// agent-main failure and carry more failed jobs than the job-log budget, so a
/// slot or log read spent on them would starve agent-main.
#[test]
fn ci_retired_ref_candidates_do_not_starve_an_agent_main_failure() {
    if !isolated_with_fake_gh(
        "sweep::ci_retired_ref_candidates_do_not_starve_an_agent_main_failure",
    ) {
        return;
    }
    let mut workspace = Workspace::new();
    let attributed = |payload: &str| {
        format!("{JOB_NAME}\t{FAILED_STEP}\t2026-10-10T06:37:01.0000000Z {payload}\n")
    };
    let log = [
        "HEAD is now at {checkout} Merge into agent-main",
        "##[group]Run cargo nextest run --workspace",
        "        FAIL [   0.120s] orbit-core::ci_failure_goldens sweep::landing_red",
        "thread 'sweep::landing_red' panicked at crates/orbit-core/tests/sweep.rs:9:5:",
        "assertion `left == right` failed",
        "error: test run failed",
        "##[error]Process completed with exit code 101.",
    ]
    .map(attributed)
    .concat();
    workspace.push_red_run(8_100, 9_200, &log);
    let retired_runs = [8_101, 8_102, 8_103, 8_104];
    for run_id in retired_runs {
        workspace.publish_red_run(
            &format!("task/{run_id}-retired"),
            "pull_request",
            "3333333333333333333333333333333333333333",
            run_id,
            &[run_id * 10, run_id * 10 + 1],
            &log,
        );
    }

    let (evidence, filed) = workspace.sweep();

    assert_eq!(evidence["retryable_errors"], json!([]), "{evidence:#}");
    let current = evidence["current_failures"].as_array().unwrap();
    assert_eq!(current.len(), 1, "{evidence:#}");
    assert_eq!(current[0]["run_id"], json!(8_100));
    assert_eq!(current[0]["investigated"], json!(true));
    assert_eq!(current[0]["diagnostic_unit"]["job_id"], json!(9_200));
    let stale = evidence["stale_or_superseded"].as_array().unwrap();
    assert_eq!(stale.len(), retired_runs.len(), "{stale:#?}");
    assert!(
        stale
            .iter()
            .all(|entry| entry["reason"] == json!("ref_no_longer_exists")),
        "{stale:#?}"
    );
    let calls = std::fs::read_to_string(workspace.state.join("calls")).unwrap();
    let log_reads: Vec<&str> = calls
        .lines()
        .filter(|call| call.contains("--log"))
        .collect();
    assert!(
        log_reads
            .iter()
            .all(|call| call.starts_with("run view 8100 ")),
        "no job-log read is spent on a retired ref: {log_reads:?}"
    );
    assert!(!log_reads.is_empty(), "{calls}");
    let output = filed.expect("the agent-main failure is filed");
    assert_eq!(output["outcome"], json!("current_failures"), "{output:#}");
    assert_eq!(open_sweep_tasks(&workspace.runtime).len(), 1);
}
