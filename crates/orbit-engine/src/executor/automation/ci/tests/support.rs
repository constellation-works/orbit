//! Scripted GitHub state.
//!
//! Collection is a pure function of what GitHub says, so the tests script that
//! end and never spawn `gh`.

use std::collections::HashMap;
use std::sync::Mutex;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::query::{AuthStatus, CiQueries, LogScope, RunLog};

/// Scripted GitHub answers. Anything not scripted is an empty result, which is
/// itself a case worth exercising.
#[derive(Default)]
pub(super) struct FakeQueries {
    pub(super) auth: Option<AuthStatus>,
    pub(super) repo: Value,
    pub(super) pull_requests: Vec<Value>,
    pub(super) branch_heads: HashMap<String, String>,
    /// Repository-wide run pages. Each `repository_runs` call pops the next
    /// page, so a test can make CI progress between calls.
    pub(super) runs: Mutex<Vec<Vec<Value>>>,
    pub(super) run_views: HashMap<String, Value>,
    pub(super) job_logs: HashMap<(String, u64, bool), String>,
    pub(super) logs: HashMap<(String, bool), String>,
    pub(super) repository_runs_error: Option<String>,
    pub(super) run_view_errors: HashMap<String, String>,
    pub(super) branch_head_errors: HashMap<String, String>,
    pub(super) log_errors: HashMap<(String, bool), String>,
    /// Logs the per-job fallback recovered, with the job identity it read.
    pub(super) job_log_fallbacks: HashMap<(String, bool), (String, Vec<Value>)>,
    /// Reads that ended with no text because the fallback recovered none.
    pub(super) log_fallback_errors: HashMap<(String, bool), String>,
}

impl FakeQueries {
    pub(super) fn authenticated() -> Self {
        Self {
            auth: Some(AuthStatus {
                available: true,
                authenticated: true,
                detail: "GitHub CLI is authenticated on this host".to_string(),
            }),
            repo: json!({
                "name": "orbit",
                "full_name": "acme/orbit",
                "default_branch": "main",
            }),
            ..Self::default()
        }
    }

    pub(super) fn unauthenticated(detail: &str) -> Self {
        Self {
            auth: Some(AuthStatus {
                available: true,
                authenticated: false,
                detail: detail.to_string(),
            }),
            ..Self::default()
        }
    }

    pub(super) fn with_head(mut self, branch: &str, sha: &str) -> Self {
        self.branch_heads
            .insert(branch.to_string(), sha.to_string());
        self
    }

    pub(super) fn with_pull_request(mut self, pull_request: Value) -> Self {
        self.pull_requests.push(pull_request);
        self
    }

    pub(super) fn with_runs(self, pages: Vec<Vec<Value>>) -> Self {
        *self.runs.lock().expect("runs lock") = pages;
        self
    }

    pub(super) fn with_run_view(mut self, run_id: &str, view: Value) -> Self {
        self.run_views.insert(run_id.to_string(), view);
        self
    }

    pub(super) fn with_repository_runs_error(mut self, message: &str) -> Self {
        self.repository_runs_error = Some(message.to_string());
        self
    }

    pub(super) fn with_branch_head_error(mut self, branch: &str, message: &str) -> Self {
        self.branch_head_errors
            .insert(branch.to_string(), message.to_string());
        self
    }

    pub(super) fn with_run_view_error(mut self, run_id: &str, message: &str) -> Self {
        self.run_view_errors
            .insert(run_id.to_string(), message.to_string());
        self
    }

    pub(super) fn with_log_error(mut self, run_id: &str, all_scope: bool, message: &str) -> Self {
        self.log_errors
            .insert((run_id.to_string(), all_scope), message.to_string());
        self
    }

    pub(super) fn with_log(mut self, run_id: &str, all_scope: bool, log: &str) -> Self {
        self.logs
            .insert((run_id.to_string(), all_scope), log.to_string());
        self
    }

    /// Script a read whose run-scoped query came back empty and whose evidence
    /// came from one job's own log instead.
    pub(super) fn with_job_log_fallback(
        mut self,
        run_id: &str,
        all_scope: bool,
        log: &str,
        jobs: Vec<Value>,
    ) -> Self {
        self.job_log_fallbacks
            .insert((run_id.to_string(), all_scope), (log.to_string(), jobs));
        self
    }

    /// Script a read that recovered nothing: the run-scoped query was empty
    /// and the fallback could not stand in for it.
    pub(super) fn with_log_fallback_error(
        mut self,
        run_id: &str,
        all_scope: bool,
        message: &str,
    ) -> Self {
        self.log_fallback_errors
            .insert((run_id.to_string(), all_scope), message.to_string());
        self
    }
}

impl CiQueries for FakeQueries {
    fn auth_status(&self) -> AuthStatus {
        self.auth.clone().unwrap_or(AuthStatus {
            available: false,
            authenticated: false,
            detail: "no GitHub CLI on this host".to_string(),
        })
    }

    fn repo_view(&self) -> Result<Value, OrbitError> {
        Ok(self.repo.clone())
    }

    fn open_pull_requests(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        Ok(self
            .pull_requests
            .iter()
            .take(limit as usize)
            .cloned()
            .collect())
    }

    fn repository_runs(&self, _limit: u64) -> Result<Vec<Value>, OrbitError> {
        if let Some(message) = &self.repository_runs_error {
            return Err(OrbitError::Execution(message.clone()));
        }
        let mut runs = self.runs.lock().expect("runs lock");
        if runs.len() > 1 {
            Ok(runs.remove(0))
        } else {
            Ok(runs.first().cloned().unwrap_or_default())
        }
    }

    fn run_view(&self, run_id: &str) -> Result<Value, OrbitError> {
        if let Some(message) = self.run_view_errors.get(run_id) {
            return Err(OrbitError::Execution(message.clone()));
        }
        Ok(self
            .run_views
            .get(run_id)
            .cloned()
            .unwrap_or_else(|| json!({"failed_jobs": []})))
    }

    fn run_logs(
        &self,
        run_id: &str,
        job_id: u64,
        scope: LogScope,
        max_bytes: usize,
    ) -> Result<RunLog, OrbitError> {
        if let Some(message) = self
            .log_errors
            .get(&(run_id.to_string(), scope == LogScope::All))
        {
            return Err(OrbitError::Execution(message.clone()));
        }
        let key = (run_id.to_string(), scope == LogScope::All);
        if let Some((raw, jobs)) = self.job_log_fallbacks.get(&key) {
            let mut log = super::super::query::bounded_run_log(raw, max_bytes);
            log.source = orbit_tools::github_cli::SOURCE_JOB_API_LOG.to_string();
            log.source_jobs = jobs.clone();
            return Ok(log);
        }
        let raw = self
            .job_logs
            .get(&(run_id.to_string(), job_id, scope == LogScope::All))
            .or_else(|| self.logs.get(&key))
            .cloned()
            .unwrap_or_default();
        let mut log = super::super::query::bounded_run_log(&raw, max_bytes);
        log.fallback_error = self.log_fallback_errors.get(&key).cloned();
        Ok(log)
    }

    fn remote_branch_head(&self, branch: &str) -> Result<Option<String>, OrbitError> {
        if let Some(message) = self.branch_head_errors.get(branch) {
            return Err(OrbitError::Execution(message.clone()));
        }
        Ok(self.branch_heads.get(branch).cloned())
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

pub(super) fn run_on_branch(
    run_id: u64,
    workflow: &str,
    branch: &str,
    sha: &str,
    status: &str,
    conclusion: Option<&str>,
    created_at: &str,
) -> Value {
    let mut value = run(run_id, workflow, sha, status, conclusion, created_at);
    value["head_branch"] = json!(branch);
    value
}

pub(super) fn failed_job(job_id: u64, name: &str) -> Value {
    json!({
        "job_id": job_id,
        "name": name,
        "conclusion": "failure",
        "url": format!("https://github.com/acme/orbit/actions/runs/1/job/{job_id}"),
        "failed_steps": [{"name": name, "conclusion": "failure"}],
    })
}
