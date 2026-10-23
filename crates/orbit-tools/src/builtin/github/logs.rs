//! Reading one bounded runner-log excerpt out of a `gh` process.
//!
//! `gh run view --log-failed` is the primary read, and it has a live blind
//! spot: for some runs it exits 0 having written nothing at all, and for a
//! workflow that is still running it can reject every run-scoped log even
//! after one of its jobs has completed unsuccessfully. The same completed
//! job's log API can still serve the full runner log. Neither response is
//! evidence that a run passed or that its logs expired, so this module makes
//! one verified recovery attempt through the job log API.
//!
//! The fallback stays inside the boundaries the primary read already has: the
//! same `gh` process contract, the same streaming collector, the same excerpt
//! and scan caps, and the same redaction. It reads only jobs that a verified
//! `gh run view` payload attributes to the requested run, and — for failed
//! scope — only jobs that run itself reported unsuccessful, so a job that
//! passed can never be returned as failed-step evidence.

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::{ExecRequest, NoSandbox, run_process, run_process_streaming_stdout};
use serde_json::{Value, json};

use crate::{TIMEOUT_LONG_MS, check_exec_result};

use super::{LogSelection, StreamedLog, StreamedLogCollector};

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
enum ExcerptShape {
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
            Some(_) => Some(super::require_numeric_id(input, "job")?),
            None => None,
        };
        Ok(Self {
            run_log: run_log_request(input)?,
            run_view: super::run::view::build_exec_request(input)?,
            run_id: super::require_numeric_id(input, "run")?,
            job,
            scope: LogScope::from_input(input)?,
            repository: super::repository_path(input)?,
            selection: log_selection_from_input(input)?,
        })
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
    fn job_log_request(&self, job_id: u64) -> ExecRequest {
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
}

/// Build the run-scoped `gh run view --log*` request.
pub(super) fn run_log_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let mut args = vec![
        "run".to_string(),
        "view".to_string(),
        super::require_numeric_id(input, "run")?,
    ];
    if input.get("job").is_some() {
        args.push("--job".to_string());
        args.push(super::require_numeric_id(input, "job")?);
    }
    super::push_repo_flag(&mut args, input)?;
    args.push(LogScope::from_input(input)?.flag().to_string());

    Ok(super::gh_exec_request(args, None, TIMEOUT_LONG_MS))
}

/// One bounded log read, and where its bytes came from.
pub struct RunLogRead {
    pub log: StreamedLog,
    /// [`SOURCE_RUN_LOG`] or [`SOURCE_JOB_API_LOG`].
    pub source: &'static str,
    /// Identity of the job whose log was read. Empty for a run-scoped read.
    pub source_jobs: Vec<Value>,
    /// Why the fallback recovered nothing, already redacted. Set only when the
    /// read ends with no text: a fallback that found evidence reports none.
    pub fallback_error: Option<String>,
    /// The requested step could not be mapped in a raw job log (see
    /// `StreamedLog::selection_unsupported`).
    pub selection_unsupported: bool,
}

/// Read one bounded log excerpt, recovering from per-job logs when the
/// run-scoped read succeeds with no output or reports the known parent-run
/// readiness error for this exact run.
///
/// Other failures stay errors: auth, network, source-limit, and retired-run
/// failures must remain visible to the caller rather than being mistaken for
/// the one GitHub readiness gap this fallback can safely answer.
///
/// `cached_run_view` lets a caller that already holds a verified, freshly
/// fetched `gh run view` payload for this exact run (projected the same way
/// `fallback_jobs` would project its own) hand it in so the fallback never
/// re-queries GitHub for metadata the caller already has.
pub fn read_run_log(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    cached_run_view: Option<&Value>,
) -> Result<RunLogRead, OrbitError> {
    read_run_log_guarded(requests, bounds, cached_run_view, &|request| {
        Ok(request.clone())
    })
}

/// Read the same bounded log, with a caller guard before every subprocess,
/// including metadata and per-job fallback reads. The guard may clamp the
/// request timeout or refuse an exhausted absolute budget.
pub fn read_run_log_guarded(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    cached_run_view: Option<&Value>,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Result<RunLogRead, OrbitError> {
    let log = match stream_bounded_log(
        &guard(&requests.run_log)?,
        bounds,
        ExcerptShape::EvenSplit,
        "gh run view --log",
        requests.selection.clone(),
    ) {
        Ok(log) => log,
        Err(error) if is_parent_run_log_readiness_error(&error, &requests.run_id) => {
            let empty = match &requests.selection {
                Some(selection) => StreamedLogCollector::scoped(
                    bounds.max_bytes,
                    bounds.max_evidence_lines,
                    selection.clone(),
                )
                .finish(),
                None => {
                    StreamedLogCollector::new(bounds.max_bytes, bounds.max_evidence_lines).finish()
                }
            };
            let read = recover_from_job_logs(
                requests,
                bounds,
                empty,
                Some(&error.to_string()),
                cached_run_view,
                guard,
            );
            return ensure_selection_found(requests, read);
        }
        Err(error) => return Err(error),
    };
    if requests.selection.is_some() {
        if log.selection_found {
            return Ok(RunLogRead {
                log,
                source: SOURCE_RUN_LOG,
                source_jobs: Vec::new(),
                fallback_error: None,
                selection_unsupported: false,
            });
        }
        if log.total_bytes > 0 {
            return Err(selection_not_found_error(
                requests,
                &log,
                None,
                log.selection_unsupported,
            ));
        }
        // An empty run-scoped read is the known blind spot: the scoped
        // selection may still be present in a verified job log.
        let read = recover_from_job_logs(requests, bounds, log, None, cached_run_view, guard);
        return ensure_selection_found(requests, read);
    }
    if !log.text.trim().is_empty() {
        return Ok(RunLogRead {
            log,
            source: SOURCE_RUN_LOG,
            source_jobs: Vec::new(),
            fallback_error: None,
            selection_unsupported: false,
        });
    }
    Ok(recover_from_job_logs(
        requests,
        bounds,
        log,
        None,
        cached_run_view,
        guard,
    ))
}

fn ensure_selection_found(
    requests: &RunLogRequests,
    read: RunLogRead,
) -> Result<RunLogRead, OrbitError> {
    if requests.selection.is_none() || read.log.selection_found {
        return Ok(read);
    }
    Err(selection_not_found_error(
        requests,
        &read.log,
        read.fallback_error.as_deref(),
        read.selection_unsupported || read.log.selection_unsupported,
    ))
}

fn selection_not_found_error(
    requests: &RunLogRequests,
    log: &StreamedLog,
    fallback_error: Option<&str>,
    selection_unsupported: bool,
) -> OrbitError {
    let Some(selection) = &requests.selection else {
        return OrbitError::InvalidInput("log selection not found".to_string());
    };
    let job = match &requests.job {
        Some(job) => format!(" job {job}"),
        None => String::new(),
    };
    let detail = if let Some(step) = &selection.step {
        if selection_unsupported {
            format!(
                "log step selection unsupported: \"{step}\" in run {}{job}: raw job logs name steps only by their `Run <command>` display group, not by workflow step name, so this step's identity and runtime boundaries cannot be reliably established in the raw log",
                requests.run_id
            )
        } else {
            format!(
                "log step not found: \"{step}\" in run {}{job}",
                requests.run_id
            )
        }
    } else if let Some((start, end)) = selection.byte_window {
        format!(
            "byte window {start}..{} matched no log bytes in run {}{job} ({} bytes read)",
            end.map(|end| end.to_string())
                .unwrap_or_else(|| "end".to_string()),
            requests.run_id,
            log.total_bytes
        )
    } else if let Some((start, end)) = selection.line_window {
        format!(
            "line window {}..{} matched no log lines in run {}{job} ({} bytes read)",
            start + 1,
            end.map(|end| (end + 1).to_string())
                .unwrap_or_else(|| "end".to_string()),
            requests.run_id,
            log.total_bytes
        )
    } else {
        format!(
            "log selection matched nothing in run {}{job}",
            requests.run_id
        )
    };
    let detail = match fallback_error {
        Some(error) => format!("{detail}; {error}"),
        None => detail,
    };
    OrbitError::InvalidInput(format!("{detail}; no log bytes returned"))
}

/// GitHub CLI's exact readiness response for a parent workflow that has not
/// completed. Including the requested numeric run in the match prevents a
/// message about some other run from authorizing the fallback.
fn is_parent_run_log_readiness_error(error: &OrbitError, run_id: &str) -> bool {
    let message = error.to_string();
    let Some(detail) = message.strip_prefix("execution failed: gh run view --log failed: ") else {
        return false;
    };
    let readiness =
        format!("run {run_id} is still in progress; logs will be available when it is complete");
    detail == readiness || detail == format!("failed to get run log: {readiness}")
}

/// Read one `gh` stdout stream up to the source cap, retaining only bounded
/// display, command, and checkout evidence. No unbounded source copy is held.
fn stream_bounded_log(
    request: &ExecRequest,
    bounds: LogReadBounds,
    shape: ExcerptShape,
    label: &str,
    selection: Option<LogSelection>,
) -> Result<StreamedLog, OrbitError> {
    let LogReadBounds {
        max_bytes,
        max_evidence_lines,
        ..
    } = bounds;
    let (result, log) = run_process_streaming_stdout(request, &NoSandbox, move |mut stdout| {
        use std::io::Read;

        let mut collector = match (shape, selection) {
            (ExcerptShape::EvenSplit, Some(selection)) => {
                StreamedLogCollector::scoped(max_bytes, max_evidence_lines, selection)
            }
            (ExcerptShape::TailWeighted, Some(selection)) => {
                StreamedLogCollector::tail_weighted_scoped(max_bytes, max_evidence_lines, selection)
            }
            (ExcerptShape::EvenSplit, None) => {
                StreamedLogCollector::new(max_bytes, max_evidence_lines)
            }
            (ExcerptShape::TailWeighted, None) => {
                StreamedLogCollector::tail_weighted(max_bytes, max_evidence_lines)
            }
        };
        let mut chunk = [0_u8; 4096];
        let mut source_bytes = 0usize;
        loop {
            let read = stdout.read(&mut chunk).map_err(|error| {
                OrbitError::Execution(format!("failed reading gh log stream: {error}"))
            })?;
            if read == 0 {
                return Ok(collector.finish());
            }
            source_bytes += read;
            if source_bytes > super::MAX_CHECKOUT_LOG_SCAN_BYTES {
                return Err(OrbitError::Execution(
                    "job log exceeded the 8 MiB source read limit; evidence is incomplete"
                        .to_string(),
                ));
            }
            collector.push(&chunk[..read]);
        }
    })?;
    check_exec_result(&result, label)?;
    Ok(log)
}

/// One job whose own log may stand in for an empty run-scoped read.
struct FallbackJob {
    id: u64,
    name: String,
    status: String,
    conclusion: String,
    url: Option<String>,
}

impl FallbackJob {
    fn to_json(&self) -> Value {
        json!({
            "job_id": self.id,
            "name": self.name,
            "status": self.status,
            "conclusion": self.conclusion,
            "url": self.url,
        })
    }

    /// How this job is named in a fallback failure message.
    fn label(&self) -> String {
        format!("job {} (`{}`)", self.id, self.name)
    }
}

/// Read per-job logs until one yields text, keeping the empty run-scoped read
/// as the result when none does.
fn recover_from_job_logs(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    empty: StreamedLog,
    primary_error: Option<&str>,
    cached_run_view: Option<&Value>,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> RunLogRead {
    let unrecovered = |reason: String, selection_unsupported: bool| RunLogRead {
        log: empty,
        source: SOURCE_RUN_LOG,
        source_jobs: Vec::new(),
        fallback_error: Some(redact_all(&reason)),
        selection_unsupported,
    };

    let qualify = |reason: String| match primary_error {
        Some(primary) => format!("{primary}; job log recovery failed: {reason}"),
        None => reason,
    };
    let jobs = match fallback_jobs(requests, cached_run_view, guard) {
        Ok(jobs) => jobs,
        Err(reason) => return unrecovered(qualify(reason), false),
    };
    let considered = jobs.len();
    let mut attempts: Vec<String> = Vec::new();
    let mut selection_unsupported = false;
    for job in jobs.iter().take(bounds.max_fallback_jobs) {
        let request = match guard(&requests.job_log_request(job.id)) {
            Ok(request) => request,
            Err(error) => return unrecovered(qualify(error.to_string()), selection_unsupported),
        };
        match stream_bounded_log(
            &request,
            bounds,
            ExcerptShape::TailWeighted,
            "gh api job logs",
            requests.selection.clone(),
        ) {
            Ok(log) if !log.text.trim().is_empty() => {
                return RunLogRead {
                    log,
                    source: SOURCE_JOB_API_LOG,
                    source_jobs: vec![job.to_json()],
                    fallback_error: None,
                    selection_unsupported: false,
                };
            }
            Ok(log) if requests.selection.is_some() => {
                selection_unsupported |= log.selection_unsupported;
                attempts.push(if log.selection_unsupported {
                    format!(
                        "{}: raw log step mapping unsupported for the requested step",
                        job.label()
                    )
                } else {
                    format!("{} did not contain the requested step/window", job.label())
                })
            }
            Ok(_) => attempts.push(format!("{} returned no log text", job.label())),
            Err(error) => attempts.push(format!("{}: {error}", job.label())),
        }
    }
    let skipped = considered.saturating_sub(bounds.max_fallback_jobs);
    let mut reason = format!(
        "job log fallback read {} of {considered} candidate job(s): {}",
        attempts.len(),
        attempts.join("; ")
    );
    if skipped > 0 {
        reason.push_str(&format!(
            "; {skipped} further job(s) were not read (max_fallback_jobs={})",
            bounds.max_fallback_jobs
        ));
    }
    unrecovered(qualify(reason), selection_unsupported)
}

/// The jobs whose logs may be read for this request, from verified run
/// metadata.
///
/// Every rejection here is deliberate: unverifiable metadata must end the
/// fallback rather than widen it. The failure is returned as prose because it
/// is evidence about *this run's* collection, not a fault the caller can
/// retry differently.
///
/// `cached_run_view` skips the self-query below entirely when the caller
/// already holds an equivalent, freshly projected payload — identity is still
/// verified against it exactly as it would be against a freshly fetched one.
fn fallback_jobs(
    requests: &RunLogRequests,
    cached_run_view: Option<&Value>,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Result<Vec<FallbackJob>, String> {
    let owned;
    let view = match cached_run_view {
        Some(view) => view,
        None => {
            let request = guard(&requests.run_view).map_err(|error| error.to_string())?;
            let result = run_process(&request, &NoSandbox)
                .map_err(|error| format!("gh run view could not run: {error}"))?;
            check_exec_result(&result, "gh run view").map_err(|error| error.to_string())?;
            owned = super::parse_gh_json(&result.stdout, "gh run view")
                .map(|parsed| super::run::view::project_run_view(&parsed))
                .map_err(|error| error.to_string())?;
            &owned
        }
    };

    // Identity first. A payload that does not name the run under investigation
    // cannot license reading anything, however well-formed its job list looks.
    match view.get("run_id").and_then(Value::as_u64) {
        Some(reported) if reported.to_string() == requests.run_id => {}
        Some(reported) => {
            return Err(format!(
                "gh run view reported run {reported}, not run {}; refusing its job metadata",
                requests.run_id
            ));
        }
        None => {
            return Err(format!(
                "gh run view reported no run id for run {}; refusing its job metadata",
                requests.run_id
            ));
        }
    }

    let jobs = match requests.scope {
        // Failed-step evidence may only come from a job the run itself
        // reported unsuccessful.
        LogScope::Failed => collect_jobs(view, "failed_jobs", requests, true),
        // Whole-run scope exists to evidence the checked-out commit, which
        // every job records for itself. Failed jobs come first: the commit
        // that matters is the one the failing job tested.
        LogScope::All => {
            let mut jobs = collect_jobs(view, "jobs", requests, true);
            jobs.extend(collect_jobs(view, "jobs", requests, false));
            jobs
        }
    };
    if jobs.is_empty() {
        return Err(match (&requests.job, requests.scope) {
            (Some(job), LogScope::Failed) => format!(
                "job {job} is not a job of run {} that this run reported unsuccessful",
                requests.run_id
            ),
            (Some(job), LogScope::All) => {
                format!("job {job} is not a job of run {}", requests.run_id)
            }
            (None, LogScope::Failed) => format!(
                "run {} reported no failed job whose log could be read",
                requests.run_id
            ),
            (None, LogScope::All) => {
                format!(
                    "run {} reported no job whose log could be read",
                    requests.run_id
                )
            }
        });
    }
    Ok(jobs)
}

/// Read one job array out of a projected run view, keeping only jobs this
/// request is allowed to read.
fn collect_jobs(
    view: &Value,
    key: &str,
    requests: &RunLogRequests,
    unsuccessful: bool,
) -> Vec<FallbackJob> {
    view.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        // A running job's API can serve a partial stream. Only a completed
        // job is evidence complete enough to diagnose or file from.
        .filter(|job| job.get("status").and_then(Value::as_str) == Some("completed"))
        .filter(|job| super::run::view::is_unsuccessful(&job["conclusion"]) == unsuccessful)
        .filter_map(|job| {
            let id = job.get("job_id").and_then(Value::as_u64)?;
            if requests
                .job
                .as_ref()
                .is_some_and(|narrowed| *narrowed != id.to_string())
            {
                return None;
            }
            let url = job.get("url").and_then(Value::as_str);
            // A job whose own URL names a different run is metadata this
            // request cannot vouch for, whatever the payload's run id said.
            if url.is_some_and(|url| url_run_id(url).is_some_and(|run| run != requests.run_id)) {
                return None;
            }
            Some(FallbackJob {
                id,
                name: job
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status: job
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                conclusion: job
                    .get("conclusion")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                url: url.map(ToOwned::to_owned),
            })
        })
        .collect()
}

/// The run a job URL belongs to, from `…/actions/runs/<run>/job/<job>`.
fn url_run_id(url: &str) -> Option<&str> {
    url.split("/runs/").nth(1)?.split('/').next()
}
