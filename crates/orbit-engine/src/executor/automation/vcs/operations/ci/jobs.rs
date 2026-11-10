//! [ORB-15956] The hosted-CI reads and the one rerun the owner's landing makes
//! when a pull request's check fails: which jobs ran on a commit, a failed
//! job's bounded log, and a rerun of a run's failed jobs.
//!
//! Missing-required prerequisite recovery uses the same rerun operation with
//! typed evidence and a durable owner reservation; this adapter rechecks its
//! exact identities, source, latest attempt and deadline before mutation.
//!
//! The log read is the CI sweep's own (`orbit_tools::github_cli`), so landing
//! classifies a failure from exactly the excerpt the sweep would file.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::github_cli;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

use super::super::DEFAULT_TIMEOUT_MS;
use super::super::input::{required_string, valid_expected_remote_sha};
use super::super::process::execute_with_transient_retry;
use super::log_answer::CiJobLogAnswer;

/// Parsed job identity for bounded diagnostics; incomplete identities remain
/// incomplete, with the same serialized optional fields as historical notes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(in crate::executor::automation::vcs) struct CiCommitJobIdentity {
    pub(in crate::executor::automation::vcs) run_id: Option<u64>,
    pub(in crate::executor::automation::vcs) job_id: Option<u64>,
    pub(in crate::executor::automation::vcs) run_attempt: Option<u64>,
}

/// A field observation keeps missing, null and malformed metadata distinct.
/// Raw bytes are used only for unchanged history projections and raw equality;
/// policy reads the parsed value, never a default identity.
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) enum CiJobObservation<T> {
    Missing,
    Null,
    Known { value: T, wire: Value },
    Malformed(Value),
}

impl<T> CiJobObservation<T> {
    pub(super) fn decode(wire: Option<Value>, parse: impl FnOnce(&Value) -> Option<T>) -> Self {
        match wire {
            None => Self::Missing,
            Some(Value::Null) => Self::Null,
            Some(wire) => match parse(&wire) {
                Some(value) => Self::Known { value, wire },
                None => Self::Malformed(wire),
            },
        }
    }

    pub(in crate::executor::automation::vcs) fn value(&self) -> Option<&T> {
        match self {
            Self::Known { value, .. } => Some(value),
            _ => None,
        }
    }

    fn wire(&self) -> &Value {
        match self {
            Self::Missing | Self::Null => &Value::Null,
            Self::Known { wire, .. } | Self::Malformed(wire) => wire,
        }
    }

    /// Historical matching used JSON indexing, which equates missing and null
    /// but distinguishes different malformed values. Keep exactly that rule.
    pub(in crate::executor::automation::vcs) fn same_observation(&self, other: &Self) -> bool {
        self.wire() == other.wire()
    }

    /// Only the legacy raw API failed-job adapter needs this comparison.
    pub(in crate::executor::automation::vcs) fn matches_legacy(&self, other: &Value) -> bool {
        self.wire() == other
    }
}

impl<T: Serialize> CiJobObservation<T> {
    fn known(value: T) -> Result<Self, OrbitError> {
        let wire = serde_json::to_value(&value)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        Ok(Self::Known { value, wire })
    }
}

impl<T> Serialize for CiJobObservation<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire().serialize(serializer)
    }
}

impl CiJobObservation<String> {
    pub(in crate::executor::automation::vcs) fn text(&self) -> Option<&str> {
        self.value().map(String::as_str)
    }
}

impl CiJobObservation<u64> {
    pub(in crate::executor::automation::vcs) fn number(&self) -> Option<u64> {
        self.value().copied()
    }
}

// Unknown provider strings remain observations, never a successful verdict.
macro_rules! ci_job_tokens {
    ($name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub(in crate::executor::automation::vcs) enum $name {
            $($variant,)+
            Unknown(String),
        }
        impl $name {
            fn parse(value: &Value) -> Option<Self> {
                Some(match value.as_str()? {
                    $($token => Self::$variant,)+
                    other => Self::Unknown(other.to_string()),
                })
            }
        }
    };
}
ci_job_tokens!(CiJobStatus { Completed => "completed" });
ci_job_tokens!(CiJobEvent { PullRequest => "pull_request" });
ci_job_tokens!(CiJobConclusion {
    Success => "success", Failure => "failure", Cancelled => "cancelled",
    TimedOut => "timed_out", ActionRequired => "action_required",
    StartupFailure => "startup_failure",
});

impl CiJobConclusion {
    pub(in crate::executor::automation::vcs) fn unsuccessful(&self) -> bool {
        matches!(
            self,
            Self::Failure
                | Self::Cancelled
                | Self::TimedOut
                | Self::ActionRequired
                | Self::StartupFailure
        )
    }

    pub(in crate::executor::automation::vcs) fn stopped(&self) -> Option<&'static str> {
        match self {
            Self::Cancelled => Some("cancelled"),
            Self::TimedOut => Some("timed_out"),
            _ => None,
        }
    }

    pub(in crate::executor::automation::vcs) fn concluded(&self) -> bool {
        !matches!(self, Self::Unknown(text) if text.is_empty())
    }
}

/// PR association accepts the adapter and old history aliases without
/// promoting malformed identity metadata to an authenticated default.
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) struct CiJobPullRequests {
    pub(in crate::executor::automation::vcs) numbers: Vec<u64>,
}
impl CiJobPullRequests {
    pub(in crate::executor::automation::vcs) fn from_legacy(job: &Value) -> Self {
        let number = |value: &Value| {
            value
                .as_u64()
                .or_else(|| value["number"].as_u64())
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        };
        let mut numbers = Vec::new();
        for key in ["pull_request_numbers", "pull_requests"] {
            if let Some(list) = job[key].as_array() {
                numbers.extend(list.iter().filter_map(number));
            }
        }
        for key in ["pr_number", "pull_request_number", "pr"] {
            if let Some(number) = number(&job[key]) {
                numbers.push(number);
            }
        }
        Self { numbers }
    }
}

fn ci_job_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}
fn ci_job_steps(value: &Value) -> Option<Vec<CiJobObservation<String>>> {
    Some(
        value
            .as_array()?
            .iter()
            .cloned()
            .map(|step| CiJobObservation::decode(Some(step), ci_job_text))
            .collect(),
    )
}

// All 21 normalized fields are decoded together. Extra fields and non-object
// rows are retained only for serialization, since existing evidence notes
// embed the complete row, including incomplete host observations.
macro_rules! ci_commit_job_fields {
    ($($field:ident : $ty:ty => $parse:expr),+ $(,)?) => {
        #[derive(Clone, Debug)]
        pub(in crate::executor::automation::vcs) struct CiCommitJobEvidence {
            $(pub(in crate::executor::automation::vcs) $field: CiJobObservation<$ty>,)+
            pub(in crate::executor::automation::vcs) association: CiJobPullRequests,
            pub(in crate::executor::automation::vcs) started_time: Option<chrono::DateTime<chrono::FixedOffset>>,
            pub(in crate::executor::automation::vcs) completed_time: Option<chrono::DateTime<chrono::FixedOffset>>,
            extra: serde_json::Map<String, Value>,
            field_order: Option<Vec<String>>,
            non_object: Option<Value>,
        }
        impl CiCommitJobEvidence {
            fn decode(value: Value) -> Self {
                let association = CiJobPullRequests::from_legacy(&value);
                let (mut fields, non_object) = match value {
                    Value::Object(fields) => (fields, None),
                    other => (serde_json::Map::new(), Some(other)),
                };
                let field_order = Some(fields.keys().cloned().collect());
                let mut row = Self {
                    $($field: CiJobObservation::decode(fields.remove(stringify!($field)), $parse),)+
                    association, started_time: None, completed_time: None,
                    extra: fields, field_order, non_object,
                };
                row.bind_times();
                row
            }
        }
        impl Serialize for CiCommitJobEvidence {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap;
                if let Some(value) = &self.non_object { return value.serialize(serializer); }
                let mut map = serializer.serialize_map(None)?;
                if let Some(order) = &self.field_order {
                    // serde_json preserves insertion order in this workspace;
                    // a decoded row must keep its original history projection.
                    for key in order {
                        match key.as_str() {
                            $(stringify!($field) => map.serialize_entry(key, &self.$field)?,)+
                            _ => if let Some(value) = self.extra.get(key) {
                                map.serialize_entry(key, value)?;
                            },
                        }
                    }
                } else {
                    $(map.serialize_entry(stringify!($field), &self.$field)?;)+
                }
                map.end()
            }
        }
    };
}
ci_commit_job_fields! {
    run_id: u64 => Value::as_u64,
    job_id: u64 => Value::as_u64,
    workflow: String => ci_job_text,
    workflow_id: u64 => Value::as_u64,
    repository_id: u64 => Value::as_u64,
    repository: String => ci_job_text,
    name: String => ci_job_text,
    status: CiJobStatus => CiJobStatus::parse,
    conclusion: CiJobConclusion => CiJobConclusion::parse,
    failed_step: String => ci_job_text,
    failed_steps: Vec<CiJobObservation<String>> => ci_job_steps,
    started_at: String => ci_job_text,
    completed_at: String => ci_job_text,
    run_status: CiJobStatus => CiJobStatus::parse,
    run_started_at: String => ci_job_text,
    run_attempt: u64 => Value::as_u64,
    event: CiJobEvent => CiJobEvent::parse,
    head_branch: String => ci_job_text,
    head_sha: String => ci_job_text,
    pull_requests: Vec<Value> => |value| value.as_array().cloned(),
    pull_request_numbers: Vec<Value> => |value| value.as_array().cloned(),
}

impl CiCommitJobEvidence {
    pub(in crate::executor::automation::vcs) fn identity(&self) -> CiCommitJobIdentity {
        CiCommitJobIdentity {
            run_id: self.run_id.number(),
            job_id: self.job_id.number(),
            run_attempt: self.run_attempt.number(),
        }
    }

    fn bind_times(&mut self) {
        self.started_time = self
            .started_at
            .text()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
        self.completed_time = self
            .completed_at
            .text()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
    }
}

/// Private ci.commit_jobs transport. A missing/malformed job list is a failed
/// read; malformed individual rows stay lenient observations for diagnostics.
#[derive(Serialize)]
pub(in crate::executor::automation::vcs) struct CiCommitJobsAnswer {
    commit: CiJobObservation<String>,
    pub(in crate::executor::automation::vcs) jobs: Vec<CiCommitJobEvidence>,
}
impl CiCommitJobsAnswer {
    pub(in crate::executor::automation::vcs) fn decode_response(response: Value) -> Option<Self> {
        let commit = CiJobObservation::decode(response.get("commit").cloned(), ci_job_text);
        let jobs = response
            .get("jobs")?
            .as_array()?
            .iter()
            .cloned()
            .map(CiCommitJobEvidence::decode)
            .collect();
        Some(Self { commit, jobs })
    }
}

/// Largest failed-job log excerpt one read returns.
const MAX_JOB_LOG_BYTES: usize = 64 * 1024;

// Project inside gh, before the process supervisor captures stdout. A page of
// repository metadata, commit messages or job-step timestamps can exceed its
// normal capture bound. Keep the original 100-row window and total_count so
// exact-tip completeness checks and bounded ancestry selection stay unchanged.
// Do not default a missing list to []: a malformed read must still fail.
const COMMIT_RUN_FIELDS: &str = "{total_count, workflow_runs: (.workflow_runs | if type != \"array\" then error(\"workflow_runs must be an array\") else map({id, name, workflow_id, repository: {id: .repository.id, full_name: .repository.full_name}, status, conclusion, run_started_at, run_attempt, event, head_branch, head_sha, pull_requests}) end)}";
const RUN_JOB_FIELDS: &str = "{total_count, jobs: (.jobs | if type != \"array\" then error(\"jobs must be an array\") else map({id, name, status, conclusion, started_at, completed_at, steps: (.steps | if type == \"array\" then map({name, number, status, conclusion}) else . end)}) end)}";

/// Every job of the newest attempt of each workflow run GitHub recorded for
/// `commit`, with its conclusion and first failed step. Base ancestry reads
/// retain completed successful pushes on the accepted base or its descendants;
/// the candidate rerun supplies the other half of attribution evidence.
pub(in crate::executor::automation::vcs::operations) fn ci_commit_jobs(
    input: &Value,
) -> Result<Value, OrbitError> {
    let commit = required_string(input, "commit")?;
    let workspace_path = Path::new(required_string(input, "workspace_path")?);
    if !valid_expected_remote_sha(Some(commit)) {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS commit jobs requires a full commit id, not '{commit}'"
        )));
    }
    // Exact-tip reads retain the latest push per workflow. Untouched-file
    // evidence reads retain the latest push per workflow and commit in the
    // accepted base's ancestry range, including superseded base tips.
    let push_branch = input.get("latest_push_branch").and_then(Value::as_str);
    let base_push_tip = input.get("base_push_tip").and_then(Value::as_str);
    if let Some(tip) = base_push_tip
        && (push_branch.is_none() || !valid_expected_remote_sha(Some(tip)))
    {
        return Err(OrbitError::InvalidInput(
            "base push history requires a branch and full tip commit id".into(),
        ));
    }
    let mut args = vec![
        "api".to_string(),
        "--method".to_string(),
        "GET".to_string(),
        "repos/{owner}/{repo}/actions/runs".to_string(),
        "-F".to_string(),
        "per_page=100".to_string(),
    ];
    if base_push_tip.is_none() {
        args.extend(["-F".into(), format!("head_sha={commit}")]);
    }
    if let Some(branch) = push_branch {
        args.extend([
            "-F".into(),
            "event=push".into(),
            "-F".into(),
            format!("branch={branch}"),
        ]);
    }
    let runs = projected_ci_json(args, workspace_path, "commit runs", COMMIT_RUN_FIELDS)?;
    let listed = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| OrbitError::Execution("commit runs carried no workflow run list".into()))?;
    let mut selected: Vec<&Value> = listed.iter().collect();
    if let Some(branch) = push_branch {
        if base_push_tip.is_none()
            && runs["total_count"]
                .as_u64()
                .is_none_or(|total| total > listed.len() as u64)
        {
            return Err(OrbitError::Execution(
                "base push runs were incomplete".into(),
            ));
        }
        let mut newest = BTreeMap::<(u64, &str), &Value>::new();
        for run in &selected {
            if run["event"] != "push" || run["head_branch"] != branch {
                continue;
            }
            let Some(sha) = run["head_sha"]
                .as_str()
                .filter(|sha| valid_expected_remote_sha(Some(sha)))
            else {
                continue;
            };
            if base_push_tip.is_none() && sha != commit {
                continue;
            }
            let (Some(workflow), Some(id)) = (run["workflow_id"].as_u64(), run["id"].as_u64())
            else {
                return Err(OrbitError::Execution(
                    "base push run omitted its identity".into(),
                ));
            };
            if newest
                .get(&(workflow, sha))
                .is_none_or(|prior| prior["id"].as_u64().is_some_and(|prior| id > prior))
            {
                newest.insert((workflow, sha), run);
            }
        }
        selected = newest.into_values().collect();
    }
    let mut jobs = Vec::new();
    for run in selected {
        // A partial or cancelled workflow cannot establish a passing base result.
        // The bounded recent-run window can miss evidence, which conservatively
        // keeps the base wait; it never treats a partial listing as a pass.
        if let Some(tip) = base_push_tip {
            if run["status"] != "completed" || run["conclusion"] != "success" {
                continue;
            }
            let sha = run["head_sha"].as_str().unwrap_or_default();
            if !super::super::super::git::git_command_success(
                workspace_path,
                &["merge-base", "--is-ancestor", commit, sha],
            )
            .unwrap_or(false)
                || !super::super::super::git::git_command_success(
                    workspace_path,
                    &["merge-base", "--is-ancestor", sha, tip],
                )
                .unwrap_or(false)
            {
                continue;
            }
        }
        let Some(run_id) = run["id"].as_u64() else {
            continue;
        };
        let listing = projected_ci_json(
            vec![
                "api".to_string(),
                "--method".to_string(),
                "GET".to_string(),
                format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs"),
                "-F".to_string(),
                "per_page=100".to_string(),
            ],
            workspace_path,
            "run jobs",
            RUN_JOB_FIELDS,
        )?;
        let listed_jobs = listing["jobs"]
            .as_array()
            .ok_or_else(|| OrbitError::Execution("run jobs carried no job list".into()))?;
        if push_branch.is_some()
            && listing["total_count"]
                .as_u64()
                .is_none_or(|total| total > listed_jobs.len() as u64)
        {
            return Err(OrbitError::Execution(
                "base push jobs were incomplete".into(),
            ));
        }
        for job in listed_jobs {
            let failed_steps: Vec<Value> = job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|step| step["conclusion"].as_str() == Some("failure"))
                .map(|step| step["name"].clone())
                .collect();
            let failed_step = failed_steps.first().and_then(Value::as_str);
            let pull_requests = if run["pull_requests"].is_array() {
                run["pull_requests"].clone()
            } else {
                json!([])
            };
            let pull_request_numbers: Vec<Value> = pull_requests
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|pull_request| {
                    pull_request
                        .as_u64()
                        .or_else(|| pull_request["number"].as_u64())
                        .map(|number| json!(number))
                })
                .collect();
            let association = CiJobPullRequests::from_legacy(&json!({
                "pull_requests":pull_requests, "pull_request_numbers":pull_request_numbers,
            }));
            let mut row = CiCommitJobEvidence {
                run_id: CiJobObservation::decode(Some(json!(run_id)), Value::as_u64),
                job_id: CiJobObservation::decode(Some(job["id"].clone()), Value::as_u64),
                workflow: CiJobObservation::decode(Some(run["name"].clone()), ci_job_text),
                workflow_id: CiJobObservation::decode(
                    Some(run["workflow_id"].clone()),
                    Value::as_u64,
                ),
                repository_id: CiJobObservation::decode(
                    Some(run["repository"]["id"].clone()),
                    Value::as_u64,
                ),
                repository: CiJobObservation::decode(
                    Some(run["repository"]["full_name"].clone()),
                    ci_job_text,
                ),
                name: CiJobObservation::decode(Some(job["name"].clone()), ci_job_text),
                status: CiJobObservation::decode(Some(job["status"].clone()), CiJobStatus::parse),
                conclusion: CiJobObservation::decode(
                    Some(job["conclusion"].clone()),
                    CiJobConclusion::parse,
                ),
                failed_step: CiJobObservation::decode(Some(json!(failed_step)), ci_job_text),
                failed_steps: CiJobObservation::decode(Some(json!(failed_steps)), ci_job_steps),
                started_at: CiJobObservation::decode(Some(job["started_at"].clone()), ci_job_text),
                completed_at: CiJobObservation::decode(
                    Some(job["completed_at"].clone()),
                    ci_job_text,
                ),
                run_status: CiJobObservation::decode(
                    Some(run["status"].clone()),
                    CiJobStatus::parse,
                ),
                run_started_at: CiJobObservation::decode(
                    Some(run["run_started_at"].clone()),
                    ci_job_text,
                ),
                run_attempt: CiJobObservation::decode(
                    Some(run["run_attempt"].clone()),
                    Value::as_u64,
                ),
                event: CiJobObservation::decode(Some(run["event"].clone()), CiJobEvent::parse),
                head_branch: CiJobObservation::decode(
                    Some(run["head_branch"].clone()),
                    ci_job_text,
                ),
                head_sha: CiJobObservation::decode(Some(run["head_sha"].clone()), ci_job_text),
                pull_requests: CiJobObservation::decode(Some(pull_requests), |value| {
                    value.as_array().cloned()
                }),
                pull_request_numbers: CiJobObservation::decode(
                    Some(json!(pull_request_numbers)),
                    |value| value.as_array().cloned(),
                ),
                association,
                started_time: None,
                completed_time: None,
                extra: serde_json::Map::new(),
                field_order: None,
                non_object: None,
            };
            row.bind_times();
            jobs.push(row);
        }
    }
    serde_json::to_value(CiCommitJobsAnswer {
        commit: CiJobObservation::known(commit.to_string())?,
        jobs,
    })
    .map_err(|error| OrbitError::Execution(error.to_string()))
}

/// One failed job's bounded, redacted failed-step log. `fallback_error` is
/// the redacted reason the read recovered no text, so the caller can tell an
/// unreadable log from a readable one; `read_throttle` beside it, or on a
/// failed primary read, is the provider's typed throttling answer. Complete command evidence or bounded
/// failure regions accompany the display with their source; callers must bind
/// them to the provider's failed job and step before recovery.
pub(in crate::executor::automation::vcs::operations) fn ci_job_log(
    input: &Value,
) -> Result<CiJobLogAnswer, OrbitError> {
    let run_id = required_string(input, "run_id")?;
    let job_id = required_string(input, "job_id")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let requests = github_cli::RunLogRequests::from_input(
        &json!({"run": run_id, "job": job_id, "scope": "failed"}),
    )?
    .in_directory(workspace_path);
    let deadline = input
        .get("deadline_ms")
        .map(|_| super::super::super::completion_deadline::Deadline::from_input(input))
        .transpose()?;
    let last_timeout = std::cell::Cell::new(DEFAULT_TIMEOUT_MS);
    let read = github_cli::read_run_log_guarded(
        &requests,
        github_cli::LogReadBounds::new(MAX_JOB_LOG_BYTES),
        None,
        &|request| {
            let mut request = request.clone();
            if let Some(deadline) = deadline {
                let timeout = deadline.remaining()?;
                last_timeout.set(timeout);
                request.timeout_ms = Some(timeout);
            }
            Ok(request)
        },
    )
    .map_err(|error| match deadline {
        Some(deadline) => deadline.type_killed_read(error, last_timeout.get()),
        None => error,
    })?;
    if let Some(deadline) = deadline {
        super::super::super::completion_deadline::probe("prerequisite log", deadline.remaining()?);
        deadline.remaining()?;
    }
    Ok(CiJobLogAnswer::from_read(read))
}

fn projected_ci_json(
    mut args: Vec<String>,
    workspace_path: &Path,
    label: &str,
    fields: &str,
) -> Result<Value, OrbitError> {
    args.extend(["--jq".into(), fields.into()]);
    let result =
        execute_with_transient_retry("gh", &args, Some(workspace_path), DEFAULT_TIMEOUT_MS, label)?;
    serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS {label} returned invalid JSON: {error}"
        ))
    })
}
