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

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::{NoSandbox, run_process, run_process_streaming_stdout};
use orbit_tools::{check_exec_result, github_cli};
use serde_json::{Value, json};

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

/// Which slice of a run's log to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogScope {
    /// Failed steps only — the working default, and where the error signature
    /// lives.
    Failed,
    /// The whole run log. The checkout step normally *succeeds*, so this is
    /// the only scope that can evidence the commit a runner actually tested.
    All,
}

impl LogScope {
    fn as_input_value(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::All => "all",
        }
    }
}

/// One run log, with a bounded human excerpt and separately bounded checkout
/// evidence extracted while the source stream is drained.
#[derive(Debug, Clone, Default)]
pub(super) struct RunLog {
    pub(super) text: String,
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

/// Cap on returned checkout-evidence lines, mirroring `github.run.logs`.
const MAX_EVIDENCE_LINES: usize = 40;

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
        Ok(github_cli::project_run_view(&github_cli::parse_gh_json(
            &stdout,
            "gh run view",
        )?))
    }

    fn run_logs(
        &self,
        run_id: &str,
        scope: LogScope,
        max_bytes: usize,
    ) -> Result<RunLog, OrbitError> {
        let mut request =
            github_cli::run_logs_request(&json!({"run": run_id, "scope": scope.as_input_value()}))?;
        request.current_dir = Some(self.repo_root.to_string_lossy().into_owned());
        let (result, log) =
            run_process_streaming_stdout(&request, &NoSandbox, move |mut stdout| {
                let mut collector =
                    github_cli::StreamedLogCollector::new(max_bytes, MAX_EVIDENCE_LINES);
                let mut chunk = [0_u8; 4096];
                loop {
                    let read = stdout.read(&mut chunk).map_err(|error| {
                        OrbitError::Execution(format!("failed reading gh run log: {error}"))
                    })?;
                    if read == 0 {
                        return Ok(collector.finish());
                    }
                    collector.push(&chunk[..read]);
                }
            })?;
        check_exec_result(&result, "gh run view --log")?;
        Ok(RunLog {
            text: log.text,
            truncated: log.truncated,
            total_bytes: log.total_bytes,
            returned_bytes: log.returned_bytes,
            checkout_commits: log.checkout_evidence.commits,
            checkout_evidence: log.checkout_evidence.lines,
            checkout_evidence_complete: log.checkout_evidence.complete,
            checkout_evidence_scanned_bytes: log.checkout_evidence.scanned_bytes,
            checkout_evidence_source_truncated: log.checkout_evidence.source_truncated,
            checkout_evidence_display_truncated: log.checkout_evidence.display_truncated,
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
    let bounded = github_cli::bound_log_text(raw, max_bytes);
    let evidence = github_cli::scan_checkout_evidence(raw, MAX_EVIDENCE_LINES);
    RunLog {
        text: bounded.text,
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
