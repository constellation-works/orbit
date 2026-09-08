//! The GitHub/git reads the CI stages make, behind one seam.
//!
//! Every call here runs on the host, unsandboxed, with whatever credentials
//! the host already has — the same boundary as `automation::vcs::operations`,
//! and for the same reason: these labels are engine-private, are never
//! advertised to agents, and do not pass through tool authorization or an
//! activity allowlist. Nothing in this module is reachable from an activity's
//! tool surface, and nothing it returns carries a credential outward.
//!
//! The argv, the JSON projections, and the log bounding all come from
//! `orbit_tools::github_cli`, so the shape of a `gh` call has exactly one
//! owner in the workspace.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::{NoSandbox, run_process};
use orbit_tools::{check_exec_result, github_cli};
use serde_json::{Value, json};

/// Which slice of a run's log to read, and the read itself, are owned by
/// `orbit_tools::github_cli` so the stages and the `github.*` tools cannot
/// drift apart on scope, bounding, or fallback.
pub(super) use github_cli::LogScope;

/// Whether a GitHub CLI exists on this host and holds usable credentials.
///
/// Neither answer is an error: a host that cannot reach GitHub has to be able
/// to report *that*, and an error return would be indistinguishable from the
/// query itself being broken.
#[derive(Debug, Clone)]
pub(in crate::executor::automation) struct AuthStatus {
    pub(in crate::executor::automation) available: bool,
    pub(in crate::executor::automation) authenticated: bool,
    pub(in crate::executor::automation) detail: String,
}

impl AuthStatus {
    pub(in crate::executor::automation) fn usable(&self) -> bool {
        self.available && self.authenticated
    }

    pub(in crate::executor::automation) fn to_json(&self) -> Value {
        json!({
            "available": self.available,
            "authenticated": self.authenticated,
            "detail": self.detail,
        })
    }
}

/// One run log, with a bounded human excerpt and separately bounded checkout
/// evidence extracted while the source stream is drained.
/// Deliberately not `Default`: a `RunLog` whose `source` is unset would carry
/// no answer to "where did these bytes come from", which is exactly the
/// question this type now exists to settle.
#[derive(Debug, Clone)]
pub(super) struct RunLog {
    /// Which query produced `text`: the run-scoped log, or one job's own log
    /// after the run-scoped read came back empty.
    pub(super) source: String,
    /// Identity of the job whose log was read, when the fallback supplied it.
    /// Primary reads are bound by the explicit job_id query argument.
    pub(super) source_jobs: Vec<Value>,
    /// Why the fallback recovered nothing. Present only when the read ends
    /// with no text at all, so the run's evidence gap can name its own cause.
    pub(super) fallback_error: Option<String>,
    pub(super) text: String,
    pub(super) diagnostic: Option<String>,
    pub(super) failure_regions: Option<Value>,
    pub(super) source_complete: bool,
    pub(super) truncated: bool,
    pub(super) total_bytes: usize,
    pub(super) returned_bytes: usize,
    pub(super) checkout_commits: Vec<String>,
    pub(super) checkout_evidence: Vec<String>,
    pub(super) checkout_evidence_complete: bool,
    pub(super) checkout_evidence_scanned_bytes: usize,
    pub(super) checkout_evidence_source_truncated: bool,
    pub(super) checkout_evidence_display_truncated: bool,
}

/// The reads the CI stages are allowed to make.
///
/// A trait rather than free functions so the stages can be exercised against
/// scripted GitHub state; the production implementation is the only one that
/// spawns a process.
pub(super) trait CiQueries {
    fn auth_status(&self) -> AuthStatus;
    /// `{"name", "full_name", "default_branch"}` — the release branch as
    /// GitHub itself reports it, never inferred from a naming convention.
    fn repo_view(&self) -> Result<Value, OrbitError>;
    fn open_pull_requests(&self, limit: u64) -> Result<Vec<Value>, OrbitError>;
    /// Recent runs across the whole repository, without a branch filter.
    fn repository_runs(&self, limit: u64) -> Result<Vec<Value>, OrbitError>;
    fn run_view(&self, run_id: &str) -> Result<Value, OrbitError>;
    fn run_logs(
        &self,
        run_id: &str,
        job_id: u64,
        scope: LogScope,
        max_bytes: usize,
    ) -> Result<RunLog, OrbitError>;
    /// Current remote head of `branch`, or `None` when the remote has no such
    /// branch. Reads `origin` without mutating anything locally.
    fn remote_branch_head(&self, branch: &str) -> Result<Option<String>, OrbitError>;
}

/// The production implementation: `gh` and `git`, run on the host.
pub(super) struct HostCiQueries {
    repo_root: PathBuf,
}

impl HostCiQueries {
    pub(super) fn new(repo_root: &Path) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
        }
    }

    fn run_gh(
        &self,
        mut request: orbit_exec::ExecRequest,
        label: &str,
    ) -> Result<String, OrbitError> {
        request.current_dir = Some(self.repo_root.to_string_lossy().into_owned());
        let result = run_process(&request, &NoSandbox)?;
        check_exec_result(&result, label)?;
        Ok(result.stdout)
    }
}

impl CiQueries for HostCiQueries {
    fn auth_status(&self) -> AuthStatus {
        let mut request = match github_cli::auth_status_request(&Value::Null) {
            Ok(request) => request,
            Err(error) => {
                return AuthStatus {
                    available: false,
                    authenticated: false,
                    detail: redact_all(&error.to_string()),
                };
            }
        };
        request.current_dir = Some(self.repo_root.to_string_lossy().into_owned());
        // A missing `gh`, or a host that refuses to execute it, surfaces as a
        // spawn error. That is a capability answer, not a fault.
        match run_process(&request, &NoSandbox) {
            Ok(result) if result.success => AuthStatus {
                available: true,
                authenticated: true,
                detail: "GitHub CLI is authenticated on this host".to_string(),
            },
            Ok(result) => AuthStatus {
                available: true,
                authenticated: false,
                detail: format!(
                    "GitHub CLI is present but holds no usable credentials on this host: {}",
                    redact_all(result.stderr.trim())
                ),
            },
            Err(error) => AuthStatus {
                available: false,
                authenticated: false,
                detail: redact_all(&error.to_string()),
            },
        }
    }

    fn repo_view(&self) -> Result<Value, OrbitError> {
        let stdout = self.run_gh(github_cli::repo_view_request(&json!({}))?, "gh repo view")?;
        Ok(github_cli::project_repo_view(&github_cli::parse_gh_json(
            &stdout,
            "gh repo view",
        )?))
    }

    fn open_pull_requests(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        let request = github_cli::pr_list_request(&json!({"state": "open", "limit": limit}))?;
        let stdout = self.run_gh(request, "gh pr list")?;
        let parsed = github_cli::parse_gh_json(&stdout, "gh pr list")?;
        Ok(parsed
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(github_cli::project_pull_request)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn repository_runs(&self, limit: u64) -> Result<Vec<Value>, OrbitError> {
        let request = github_cli::run_list_request(&json!({"limit": limit}))?;
        let stdout = self.run_gh(request, "gh run list")?;
        let parsed = github_cli::parse_gh_json(&stdout, "gh run list")?;
        Ok(parsed
            .as_array()
            .map(|entries| entries.iter().map(github_cli::project_run).collect())
            .unwrap_or_default())
    }

    fn run_view(&self, run_id: &str) -> Result<Value, OrbitError> {
        let request = github_cli::run_view_request(&json!({"run": run_id}))?;
        let stdout = self.run_gh(request, "gh run view")?;
        let view =
            github_cli::project_run_view(&github_cli::parse_gh_json(&stdout, "gh run view")?);
        if view
            .get("run_id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string())
            .as_deref()
            != Some(run_id)
        {
            return Err(OrbitError::Execution(
                "gh run view returned a different or missing run identity".to_string(),
            ));
        }
        Ok(view)
    }

    fn run_logs(
        &self,
        run_id: &str,
        job_id: u64,
        scope: LogScope,
        max_bytes: usize,
    ) -> Result<RunLog, OrbitError> {
        let requests = github_cli::RunLogRequests::from_input(
            &json!({"run": run_id, "job": job_id, "scope": scope.as_str()}),
        )?
        .in_directory(&self.repo_root.to_string_lossy());
        let read = github_cli::read_run_log(&requests, github_cli::LogReadBounds::new(max_bytes))?;

        Ok(RunLog {
            source: read.source.to_string(),
            source_jobs: read.source_jobs,
            fallback_error: read.fallback_error,
            text: read.log.text,
            diagnostic: read.log.diagnostic,
            failure_regions: read.log.failure_regions,
            source_complete: read.log.source_complete,
            truncated: read.log.truncated,
            total_bytes: read.log.total_bytes,
            returned_bytes: read.log.returned_bytes,
            checkout_commits: read.log.checkout_evidence.commits,
            checkout_evidence: read.log.checkout_evidence.lines,
            checkout_evidence_complete: read.log.checkout_evidence.complete,
            checkout_evidence_scanned_bytes: read.log.checkout_evidence.scanned_bytes,
            checkout_evidence_source_truncated: read.log.checkout_evidence.source_truncated,
            checkout_evidence_display_truncated: read.log.checkout_evidence.display_truncated,
        })
    }

    fn remote_branch_head(&self, branch: &str) -> Result<Option<String>, OrbitError> {
        let output = super::super::vcs::git::git_output(
            &self.repo_root,
            &["ls-remote", "--heads", "origin", "--", branch],
        )?;
        Ok(output
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .filter(|sha| !sha.is_empty())
            .map(ToOwned::to_owned))
    }
}

/// Bound and redact one test fixture log, scanning it before truncation.
/// Production log collection uses [`github_cli::StreamedLogCollector`] so it
/// does not retain an unbounded `gh` stdout value.
#[cfg(test)]
pub(super) fn bounded_run_log(raw: &str, max_bytes: usize) -> RunLog {
    let mut collector =
        github_cli::StreamedLogCollector::new(max_bytes, github_cli::MAX_EVIDENCE_LINES);
    collector.push(raw.as_bytes());
    let bounded = collector.finish();
    let evidence = bounded.checkout_evidence;
    RunLog {
        source: github_cli::SOURCE_RUN_LOG.to_string(),
        source_jobs: Vec::new(),
        fallback_error: None,
        text: bounded.text,
        diagnostic: bounded.diagnostic,
        failure_regions: bounded.failure_regions,
        source_complete: bounded.source_complete,
        truncated: bounded.truncated,
        total_bytes: bounded.total_bytes,
        returned_bytes: bounded.returned_bytes,
        checkout_commits: evidence.commits,
        checkout_evidence: evidence.lines,
        checkout_evidence_complete: evidence.complete,
        checkout_evidence_scanned_bytes: evidence.scanned_bytes,
        checkout_evidence_source_truncated: evidence.source_truncated,
        checkout_evidence_display_truncated: evidence.display_truncated,
    }
}
