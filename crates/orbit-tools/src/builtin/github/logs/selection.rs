use super::super::LogSelection;
use super::super::log_cache::LogCache;
use crate::TIMEOUT_LONG_MS;
use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::Value;
/// Cap on returned checkout-evidence lines. Evidence is a handful of lines per
/// job; a much larger match set means the pattern caught noise, not evidence.
pub const MAX_EVIDENCE_LINES: usize = 40;

/// How many of a run's jobs the fallback may read before it gives up. One
/// whole-job log is already a large read; this bounds the worst case to a
/// handful of them and keeps the extra query cost proportional to the run.
const DEFAULT_MAX_FALLBACK_JOBS: usize = 3;

/// The bytes came from the run-scoped `gh run view --log*` read.
pub const SOURCE_RUN_LOG: &str = "run_log";
/// The bytes came from one job's log API, because the run-scoped read had the
/// known empty/readiness blind spot.
pub const SOURCE_JOB_API_LOG: &str = "job_api_log";

/// How the excerpt budget is divided between the head and the tail of a
/// source, which depends on what that source looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExcerptShape {
    /// A failed-step log: relevant throughout, so keep both ends evenly.
    EvenSplit,
    /// A whole-job log: setup at the top, the failure at the bottom.
    TailWeighted,
}

/// Which slice of a run's logs to read.
///
/// `Failed` is the working default. `All` exists because the checkout step
/// normally *succeeds*, so the commit a runner actually tested is absent from
/// the failed-step log and only `All` can evidence it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogScope {
    Failed,
    All,
}

impl LogScope {
    /// The `gh run view` flag that selects this slice.
    fn flag(self) -> &'static str {
        match self {
            Self::Failed => "--log-failed",
            Self::All => "--log",
        }
    }

    /// The wire value callers pass in, and that snapshots report back.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::All => "all",
        }
    }

    pub fn from_input(input: &Value) -> Result<Self, OrbitError> {
        match input.get("scope").and_then(Value::as_str) {
            None | Some("failed") => Ok(Self::Failed),
            Some("all") => Ok(Self::All),
            Some(other) => Err(OrbitError::InvalidInput(format!(
                "invalid `scope`: \"{other}\"; must be \"failed\" or \"all\""
            ))),
        }
    }
}

/// The caps one log read honors, whichever source it ends up reading.
#[derive(Debug, Clone, Copy)]
pub struct LogReadBounds {
    pub max_bytes: usize,
    pub max_evidence_lines: usize,
    pub max_fallback_jobs: usize,
}

impl LogReadBounds {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            max_evidence_lines: MAX_EVIDENCE_LINES,
            max_fallback_jobs: DEFAULT_MAX_FALLBACK_JOBS,
        }
    }
}

/// The `gh` invocations one bounded log read may need, shaped up front.
///
/// Every request carries `current_dir: None` until [`Self::in_directory`] is
/// called — `gh` otherwise resolves the repository from the caller's working
/// directory.
pub struct RunLogRequests {
    /// `gh run view <run> [--job <job>] --log-failed|--log`.
    pub run_log: ExecRequest,
    /// `gh run view <run> --json …`, run only when `run_log` produced no
    /// output or hit the narrowly classified in-progress readiness response,
    /// to learn which completed jobs may stand in for it.
    pub run_view: ExecRequest,
    pub run_id: String,
    /// A single job the caller narrowed the read to, if any.
    pub job: Option<String>,
    pub scope: LogScope,
    /// `owner/name`, or gh's `{owner}/{repo}` placeholders, for the job log
    /// endpoint.
    pub repository: String,
    /// Optional step/window selection applied to the log excerpt.
    pub selection: Option<LogSelection>,
    /// Where completed logs are kept between reads. `None` reads every time.
    pub(super) cache: Option<LogCache>,
}

fn optional_usize(input: &Value, key: &str) -> Result<Option<usize>, OrbitError> {
    let Some(value) = input.get(key) else {
        return Ok(None);
    };
    let raw = match value {
        Value::Null => return Ok(None),
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("`{key}` must be a non-negative integer")))?;
    usize::try_from(raw)
        .map(Some)
        .map_err(|_| OrbitError::InvalidInput(format!("`{key}` must be a non-negative integer")))
}

/// Parse the optional `step` / byte-window / line-window selection.
///
/// Byte offsets are 0-based and end-exclusive. Line numbers are 1-based and
/// end-exclusive (`line_start: 2, line_end: 4` returns lines 2 and 3). A
/// byte window and a line window cannot be combined; either may be combined
/// with `step`, in which case it applies within the step section.
pub fn log_selection_from_input(input: &Value) -> Result<Option<LogSelection>, OrbitError> {
    let step = match input.get("step") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            let step = text.trim();
            if step.is_empty() || step.contains(['\t', '\n', '\r']) {
                return Err(OrbitError::InvalidInput(
                    "`step` must be a non-empty step name without tabs or newlines".to_string(),
                ));
            }
            Some(step.to_string())
        }
        Some(_) => {
            return Err(OrbitError::InvalidInput(
                "`step` must be a string".to_string(),
            ));
        }
    };
    let byte_start = optional_usize(input, "byte_start")?;
    let byte_end = optional_usize(input, "byte_end")?;
    let line_start_raw = optional_usize(input, "line_start")?;
    let line_end_raw = optional_usize(input, "line_end")?;
    if (byte_start.is_some() || byte_end.is_some())
        && (line_start_raw.is_some() || line_end_raw.is_some())
    {
        return Err(OrbitError::InvalidInput(
            "`byte_start`/`byte_end` cannot be combined with `line_start`/`line_end`".to_string(),
        ));
    }
    let byte_window = if byte_start.is_some() || byte_end.is_some() {
        let start = byte_start.unwrap_or(0);
        if let Some(end) = byte_end
            && end <= start
        {
            return Err(OrbitError::InvalidInput(
                "`byte_end` must be greater than `byte_start`".to_string(),
            ));
        }
        Some((start, byte_end))
    } else {
        None
    };
    let line_window = if line_start_raw.is_some() || line_end_raw.is_some() {
        let start = line_start_raw.unwrap_or(1);
        if start == 0 {
            return Err(OrbitError::InvalidInput(
                "`line_start` must be at least 1".to_string(),
            ));
        }
        if let Some(end) = line_end_raw
            && end <= start
        {
            return Err(OrbitError::InvalidInput(
                "`line_end` must be greater than `line_start`".to_string(),
            ));
        }
        Some((start - 1, line_end_raw.map(|end| end - 1)))
    } else {
        None
    };
    if step.is_none() && byte_window.is_none() && line_window.is_none() {
        return Ok(None);
    }
    Ok(Some(LogSelection {
        step,
        byte_window,
        line_window,
    }))
}

impl RunLogRequests {
    pub fn from_input(input: &Value) -> Result<Self, OrbitError> {
        let job = match input.get("job") {
            Some(_) => Some(super::super::require_numeric_id(input, "job")?),
            None => None,
        };
        Ok(Self {
            run_log: run_log_request(input)?,
            run_view: super::super::run::view::build_exec_request(input)?,
            run_id: super::super::require_numeric_id(input, "run")?,
            job,
            scope: LogScope::from_input(input)?,
            repository: super::super::repository_path(input)?,
            selection: log_selection_from_input(input)?,
            cache: None,
        })
    }

    /// Serve repeat reads of a completed job or run from `cache`.
    pub(crate) fn with_log_cache(mut self, cache: Option<LogCache>) -> Self {
        self.cache = cache;
        self
    }

    /// Serve repeat reads of a completed job or run from a test cache under `dir`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_log_cache(self, dir: &std::path::Path) -> Self {
        self.with_log_cache(Some(LogCache::in_root(dir)))
    }

    /// Run every request from `dir`, the checkout the query is about.
    pub fn in_directory(mut self, dir: &str) -> Self {
        self.run_log.current_dir = Some(dir.to_string());
        self.run_view.current_dir = Some(dir.to_string());
        self
    }

    /// One job's own log, as the log API serves it.
    ///
    /// Derived from the run-log request so the fallback keeps the same CLI,
    /// checkout, timeout, and environment as the read it stands in for; only
    /// the argv differs. `job_id` is a `u64` by construction, so nothing a
    /// caller supplied is interpolated into the endpoint path.
    pub(super) fn job_log_request(&self, job_id: u64) -> ExecRequest {
        ExecRequest {
            args: vec![
                "api".to_string(),
                "--method".to_string(),
                "GET".to_string(),
                format!("repos/{}/actions/jobs/{job_id}/logs", self.repository),
            ],
            ..self.run_log.clone()
        }
    }

    /// One `gh api` metadata read, shaped like [`Self::job_log_request`]: it
    /// keeps the run-log request's CLI, checkout, timeout and environment.
    pub(super) fn api_request(&self, endpoint: String) -> ExecRequest {
        ExecRequest {
            args: vec![
                "api".to_string(),
                "--method".to_string(),
                "GET".to_string(),
                endpoint,
            ],
            ..self.run_log.clone()
        }
    }
}

/// Build the run-scoped `gh run view --log*` request.
pub(in crate::builtin::github) fn run_log_request(
    input: &Value,
) -> Result<ExecRequest, OrbitError> {
    let mut args = vec![
        "run".to_string(),
        "view".to_string(),
        super::super::require_numeric_id(input, "run")?,
    ];
    if input.get("job").is_some() {
        args.push("--job".to_string());
        args.push(super::super::require_numeric_id(input, "job")?);
    }
    super::super::push_repo_flag(&mut args, input)?;
    args.push(LogScope::from_input(input)?.flag().to_string());

    Ok(super::super::gh_exec_request(args, None, TIMEOUT_LONG_MS))
}

/// Bind a whole-run `gh run view --log*` request to one `attempt`, so a
/// concurrent rerun cannot switch the downloaded log behind `gh`.
pub(super) fn attempt_bound_run_log_request(request: &ExecRequest, attempt: u64) -> ExecRequest {
    let mut bound = request.clone();
    let insert_at = if bound
        .args
        .last()
        .is_some_and(|flag| flag == "--log" || flag == "--log-failed")
    {
        bound.args.len() - 1
    } else {
        bound.args.len()
    };
    bound.args.splice(
        insert_at..insert_at,
        ["--attempt".to_string(), attempt.to_string()],
    );
    bound
}
