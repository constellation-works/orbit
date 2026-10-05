use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::{NoSandbox, run_process};
use orbit_tools::{check_exec_result, github_cli};
use serde_json::{Value, json};

use super::super::ci::AuthStatus;

#[derive(Debug)]
pub(super) enum AlertQuery {
    Alerts(Vec<Value>),
    CapabilityUnavailable(String),
}

pub(super) trait DependabotQueries {
    fn auth_status(&self) -> AuthStatus;
    fn repo_view(&self, repo: Option<&str>) -> Result<Value, OrbitError>;
    fn open_alerts(&self, repo: Option<&str>, limit: u64) -> Result<AlertQuery, OrbitError>;
    fn open_code_scanning_alerts(
        &self,
        repo: Option<&str>,
        limit: u64,
    ) -> Result<AlertQuery, OrbitError>;
    fn open_secret_scanning_alerts(
        &self,
        repo: Option<&str>,
        limit: u64,
    ) -> Result<AlertQuery, OrbitError>;
    fn secret_scanning_locations(
        &self,
        repo: Option<&str>,
        alert_number: u64,
        limit: u64,
    ) -> Result<Vec<Value>, OrbitError>;
    fn open_dependabot_pull_requests(
        &self,
        repo: Option<&str>,
        limit: u64,
    ) -> Result<Vec<Value>, OrbitError>;
}

pub(super) struct HostDependabotQueries {
    repo_root: PathBuf,
}

impl HostDependabotQueries {
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

    fn query_alert_family(
        &self,
        mut request: orbit_exec::ExecRequest,
        family: &str,
        project: fn(&Value) -> Value,
    ) -> Result<AlertQuery, OrbitError> {
        request.current_dir = Some(self.repo_root.to_string_lossy().into_owned());
        let result = run_process(&request, &NoSandbox)?;
        if !result.success {
            return Ok(AlertQuery::CapabilityUnavailable(classify_alert_failure(
                family,
                &result.stderr,
            )));
        }
        let parsed = github_cli::parse_gh_json(&result.stdout, family)?;
        let entries = parsed.as_array().ok_or_else(|| {
            OrbitError::Execution(format!(
                "{family} returned a non-array response; refusing to report no open alerts"
            ))
        })?;
        Ok(AlertQuery::Alerts(entries.iter().map(project).collect()))
    }
}

impl DependabotQueries for HostDependabotQueries {
    fn auth_status(&self) -> AuthStatus {
        let mut request = match github_cli::auth_status_request(&Value::Null) {
            Ok(request) => request,
            Err(error) => {
                return AuthStatus {
                    available: false,
                    authenticated: false,
                    detail: format!(
                        "GitHub CLI is unavailable on this host: {}",
                        redact_all(&error.to_string())
                    ),
                };
            }
        };
        request.current_dir = Some(self.repo_root.to_string_lossy().into_owned());
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
                detail: format!(
                    "GitHub CLI is unavailable on this host: {}",
                    redact_all(&error.to_string())
                ),
            },
        }
    }

    fn repo_view(&self, repo: Option<&str>) -> Result<Value, OrbitError> {
        let input = repo.map_or(Value::Null, |repo| json!({"repo": repo}));
        let request = github_cli::repo_view_request(&input)?;
        let stdout = self.run_gh(request, "gh repo view for Dependabot sweep")?;
        let parsed = github_cli::parse_gh_json(&stdout, "gh repo view")?;
        Ok(github_cli::project_repo_view(&parsed))
    }

    fn open_alerts(&self, repo: Option<&str>, limit: u64) -> Result<AlertQuery, OrbitError> {
        let input = json!({"repo": repo, "limit": limit});
        self.query_alert_family(
            github_cli::dependabot_alerts_request(&input)?,
            "gh api Dependabot alerts",
            github_cli::project_dependabot_alert,
        )
    }

    fn open_code_scanning_alerts(
        &self,
        repo: Option<&str>,
        limit: u64,
    ) -> Result<AlertQuery, OrbitError> {
        let input = json!({"repo": repo, "limit": limit});
        self.query_alert_family(
            github_cli::code_scanning_alerts_request(&input)?,
            "gh api Code scanning alerts",
            github_cli::project_code_scanning_alert,
        )
    }

    fn open_secret_scanning_alerts(
        &self,
        repo: Option<&str>,
        limit: u64,
    ) -> Result<AlertQuery, OrbitError> {
        let input = json!({"repo": repo, "limit": limit});
        self.query_alert_family(
            github_cli::secret_scanning_alerts_request(&input)?,
            "gh api secret scanning alerts",
            github_cli::project_secret_scanning_alert,
        )
    }

    fn secret_scanning_locations(
        &self,
        repo: Option<&str>,
        alert_number: u64,
        limit: u64,
    ) -> Result<Vec<Value>, OrbitError> {
        let input = json!({"repo": repo, "alert_number": alert_number, "limit": limit});
        let request = github_cli::secret_scanning_locations_request(&input)?;
        let stdout = self.run_gh(request, "gh api secret scanning alert locations")?;
        let parsed = github_cli::parse_gh_json(&stdout, "gh api secret scanning alert locations")?;
        let entries = parsed.as_array().ok_or_else(|| {
            OrbitError::Execution(
                "gh api secret scanning alert locations returned a non-array response".to_string(),
            )
        })?;
        Ok(entries
            .iter()
            .map(github_cli::project_secret_location)
            .collect())
    }

    fn open_dependabot_pull_requests(
        &self,
        repo: Option<&str>,
        limit: u64,
    ) -> Result<Vec<Value>, OrbitError> {
        let input = json!({"repo": repo, "limit": limit});
        let request = github_cli::dependabot_pull_requests_request(&input)?;
        let stdout = self.run_gh(request, "gh pr list for Dependabot sweep")?;
        let parsed = github_cli::parse_gh_json(&stdout, "gh pr list")?;
        let entries = parsed.as_array().ok_or_else(|| {
            OrbitError::Execution(
                "gh pr list for Dependabot sweep returned a non-array response".to_string(),
            )
        })?;
        Ok(entries
            .iter()
            .map(github_cli::project_dependabot_pull_request)
            .collect())
    }
}

fn classify_alert_failure(family: &str, stderr: &str) -> String {
    let detail = redact_all(stderr.trim());
    let lower = detail.to_ascii_lowercase();
    if lower.contains("403") {
        return format!(
            "GitHub returned HTTP 403 for {family}; the host token lacks the required repository security permission: {detail}"
        );
    }
    if lower.contains("404") {
        return format!(
            "GitHub returned HTTP 404 for {family}; the feature is disabled or unavailable for this repository: {detail}"
        );
    }
    format!("{family} could not be queried; refusing to report zero findings: {detail}")
}
