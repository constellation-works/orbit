//! Scripted GitHub state.
//!
//! Collection is a pure function of what GitHub says, so the tests script that
//! end and never spawn `gh`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::query::{AuthStatus, CiQueries, LogScope, RemoteBranchHeads, RunLog};

/// Scripted GitHub answers. Anything not scripted is an empty result.
#[derive(Default)]
pub(super) struct FakeQueries {
    repo: Value,
    branch_heads: BTreeMap<String, String>,
    /// Repository-wide run pages. Each `repository_runs` call pops the next
    /// page, so a test can make CI progress between calls.
    runs: Mutex<Vec<Vec<Value>>>,
    /// `failed_jobs` per run id; an unscripted run has none.
    failed_jobs: BTreeMap<String, Value>,
    open_pull_requests: Vec<Value>,
    closed_pull_requests: Vec<Value>,
    /// Concurrency-cancellation annotation per job id.
    concurrency_cancellations: BTreeMap<u64, String>,
    /// Failed-step logs per job id; an unscripted job's log is empty.
    logs: BTreeMap<u64, RunLog>,
    /// Every job whose log was read.
    pub(super) log_reads: Mutex<Vec<u64>>,
    /// Every limit passed to `open_pull_requests` and `repository_runs`.
    pub(super) pull_request_limits: Mutex<Vec<u64>>,
    pub(super) run_limits: Mutex<Vec<u64>>,
}

impl FakeQueries {
    pub(super) fn authenticated() -> Self {
        Self {
            repo: json!({
                "name": "orbit",
                "full_name": "acme/orbit",
                "default_branch": "main",
            }),
            ..Self::default()
        }
    }

    pub(super) fn with_head(mut self, branch: &str, sha: &str) -> Self {
        self.branch_heads
            .insert(branch.to_string(), sha.to_string());
        self
    }

    pub(super) fn with_runs(self, pages: Vec<Vec<Value>>) -> Self {
        *self.runs.lock().expect("runs lock") = pages;
        self
    }

    pub(super) fn with_failed_jobs(mut self, run_id: u64, failed_jobs: Value) -> Self {
        self.failed_jobs.insert(run_id.to_string(), failed_jobs);
        self
    }

    /// A pull request as `gh pr list` projects it.
    pub(super) fn with_pull_request(
        mut self,
        state: &str,
        number: u64,
        branch: &str,
        sha: &str,
    ) -> Self {
        let pull_request = json!({
            "number": number,
            "state": state,
            "head_branch": branch,
            "reported_head_sha": sha,
            "url": format!("https://github.com/acme/orbit/pull/{number}"),
        });
        if state == "OPEN" {
            self.open_pull_requests.push(pull_request);
        } else {
            self.closed_pull_requests.push(pull_request);
        }
        self
    }

    pub(super) fn with_concurrency_cancellation(mut self, job_id: u64, annotation: &str) -> Self {
        self.concurrency_cancellations
            .insert(job_id, annotation.to_string());
        self
    }

    /// A failed-step log whose source GitHub did not deliver in full.
    pub(super) fn with_incomplete_log(mut self, job_id: u64, text: &str) -> Self {
        let mut log = super::super::query::bounded_run_log(text, 16_384);
        log.source_complete = false;
        self.logs.insert(job_id, log);
        self
    }
}

impl CiQueries for FakeQueries {
    fn auth_status(&self) -> AuthStatus {
        AuthStatus {
            available: true,
            authenticated: true,
            detail: "GitHub CLI is authenticated on this host".to_string(),
        }
    }

    fn repo_view(&self) -> Result<Value, OrbitError> {
        Ok(self.repo.clone())
    }

    fn open_pull_requests(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        self.pull_request_limits
            .lock()
            .expect("pull request limits lock")
            .push(limit);
        Ok(self.open_pull_requests.clone())
    }

    fn closed_pull_requests(&self, _limit: u64) -> Result<Vec<Value>, OrbitError> {
        Ok(self.closed_pull_requests.clone())
    }

    fn job_concurrency_cancellation(&self, job_id: u64) -> Result<Option<String>, OrbitError> {
        Ok(self.concurrency_cancellations.get(&job_id).cloned())
    }

    fn repository_runs(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        self.run_limits.lock().expect("run limits lock").push(limit);
        let mut runs = self.runs.lock().expect("runs lock");
        if runs.len() > 1 {
            Ok(runs.remove(0))
        } else {
            Ok(runs.first().cloned().unwrap_or_default())
        }
    }

    fn run_view(&self, run_id: &str) -> Result<Value, OrbitError> {
        let failed_jobs = self.failed_jobs.get(run_id).cloned();
        Ok(json!({"failed_jobs": failed_jobs.unwrap_or_else(|| json!([]))}))
    }

    fn run_logs(
        &self,
        _run_id: &str,
        job_id: u64,
        _scope: LogScope,
        max_bytes: usize,
        _cached_view: Option<&Value>,
    ) -> Result<RunLog, OrbitError> {
        self.log_reads.lock().expect("log reads lock").push(job_id);
        Ok(self
            .logs
            .get(&job_id)
            .cloned()
            .unwrap_or_else(|| super::super::query::bounded_run_log("", max_bytes)))
    }

    fn remote_branch_heads(&self) -> Result<RemoteBranchHeads, OrbitError> {
        Ok(RemoteBranchHeads::from_scripted(
            self.branch_heads.clone(),
            BTreeMap::new(),
        ))
    }
}

pub(super) fn run(
    run_id: u64,
    workflow: &str,
    sha: &str,
    status: &str,
    conclusion: Option<&str>,
    created_at: &str,
) -> Value {
    json!({
        "run_id": run_id,
        "workflow": workflow,
        "title": format!("{workflow} on {sha}"),
        "status": status,
        "conclusion": conclusion,
        "event": "push",
        "head_branch": "topic",
        "reported_head_sha": sha,
        "created_at": created_at,
        "url": format!("https://github.com/acme/orbit/actions/runs/{run_id}"),
    })
}

pub(super) const HEAD: &str = "1111111111111111111111111111111111111111";

pub(super) fn input() -> Value {
    json!({"integration_branch": "topic", "max_checkout_log_reads": 1})
}
