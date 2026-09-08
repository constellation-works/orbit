//! Reading one bounded runner-log excerpt out of a `gh` process.
//!
//! `gh run view --log-failed` is the primary read, and it has a live blind
//! spot: for some runs it exits 0 having written nothing at all, while the
//! same run's per-job log API still serves the full runner log. An empty
//! successful read is therefore not evidence that a run passed or that its
//! logs expired, and this module recovers from the job log API rather than
//! reporting nothing.
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

use super::{StreamedLog, StreamedLogCollector};

/// Cap on returned checkout-evidence lines. Evidence is a handful of lines per
/// job; a much larger match set means the pattern caught noise, not evidence.
pub const MAX_EVIDENCE_LINES: usize = 40;

/// How many of a run's jobs the fallback may read before it gives up. One
/// whole-job log is already a large read; this bounds the worst case to a
/// handful of them and keeps the extra query cost proportional to the run.
const DEFAULT_MAX_FALLBACK_JOBS: usize = 3;

/// The bytes came from the run-scoped `gh run view --log*` read.
pub const SOURCE_RUN_LOG: &str = "run_log";
/// The bytes came from one job's log API, because the run-scoped read
/// succeeded with no output.
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
    /// output, to learn which jobs may stand in for it.
    pub run_view: ExecRequest,
    pub run_id: String,
    /// A single job the caller narrowed the read to, if any.
    pub job: Option<String>,
    pub scope: LogScope,
    /// `owner/name`, or gh's `{owner}/{repo}` placeholders, for the job log
    /// endpoint.
    pub repository: String,
}

impl RunLogRequests {
    pub fn from_input(input: &Value) -> Result<Self, OrbitError> {
        let job = match input.get("job") {
            Some(_) => Some(super::require_numeric_id(input, "job")?),
            None => None,
        };
        Ok(Self {
            run_log: run_log_request(input)?,
            run_view: super::run_view::build_exec_request(input)?,
            run_id: super::require_numeric_id(input, "run")?,
            job,
            scope: LogScope::from_input(input)?,
            repository: super::repository_path(input)?,
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
}

/// Read one bounded log excerpt, recovering from per-job logs when the
/// run-scoped read succeeds with no output.
///
/// A failing run-scoped read is still an error: it names a real problem
/// (auth, network, a retired run) that the caller must be able to retry on.
/// Only the empty-but-successful case is ambiguous enough to be worth a
/// second, verified query.
pub fn read_run_log(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
) -> Result<RunLogRead, OrbitError> {
    let log = stream_bounded_log(
        &requests.run_log,
        bounds,
        ExcerptShape::EvenSplit,
        "gh run view --log",
    )?;
    if !log.text.trim().is_empty() {
        return Ok(RunLogRead {
            log,
            source: SOURCE_RUN_LOG,
            source_jobs: Vec::new(),
            fallback_error: None,
        });
    }
    Ok(recover_from_job_logs(requests, bounds, log))
}

/// Read one `gh` stdout stream up to the source cap, retaining only bounded
/// display, command, and checkout evidence. No unbounded source copy is held.
fn stream_bounded_log(
    request: &ExecRequest,
    bounds: LogReadBounds,
    shape: ExcerptShape,
    label: &str,
) -> Result<StreamedLog, OrbitError> {
    let LogReadBounds {
        max_bytes,
        max_evidence_lines,
        ..
    } = bounds;
    let (result, log) = run_process_streaming_stdout(request, &NoSandbox, move |mut stdout| {
        use std::io::Read;

        let mut collector = match shape {
            ExcerptShape::EvenSplit => StreamedLogCollector::new(max_bytes, max_evidence_lines),
            ExcerptShape::TailWeighted => {
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
    conclusion: String,
    url: Option<String>,
}

impl FallbackJob {
    fn to_json(&self) -> Value {
        json!({
            "job_id": self.id,
            "name": self.name,
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
) -> RunLogRead {
    let unrecovered = |reason: String| RunLogRead {
        log: empty,
        source: SOURCE_RUN_LOG,
        source_jobs: Vec::new(),
        fallback_error: Some(redact_all(&reason)),
    };

    let jobs = match fallback_jobs(requests) {
        Ok(jobs) => jobs,
        Err(reason) => return unrecovered(reason),
    };
    let considered = jobs.len();
    let mut attempts: Vec<String> = Vec::new();
    for job in jobs.iter().take(bounds.max_fallback_jobs) {
        let request = requests.job_log_request(job.id);
        match stream_bounded_log(
            &request,
            bounds,
            ExcerptShape::TailWeighted,
            "gh api job logs",
        ) {
            Ok(log) if !log.text.trim().is_empty() => {
                return RunLogRead {
                    log,
                    source: SOURCE_JOB_API_LOG,
                    source_jobs: vec![job.to_json()],
                    fallback_error: None,
                };
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
    unrecovered(reason)
}

/// The jobs whose logs may be read for this request, from verified run
/// metadata.
///
/// Every rejection here is deliberate: unverifiable metadata must end the
/// fallback rather than widen it. The failure is returned as prose because it
/// is evidence about *this run's* collection, not a fault the caller can
/// retry differently.
fn fallback_jobs(requests: &RunLogRequests) -> Result<Vec<FallbackJob>, String> {
    let result = run_process(&requests.run_view, &NoSandbox)
        .map_err(|error| format!("gh run view could not run: {error}"))?;
    check_exec_result(&result, "gh run view").map_err(|error| error.to_string())?;
    let view = super::parse_gh_json(&result.stdout, "gh run view")
        .map(|parsed| super::run_view::project_run_view(&parsed))
        .map_err(|error| error.to_string())?;

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
        LogScope::Failed => collect_jobs(&view, "failed_jobs", requests, true),
        // Whole-run scope exists to evidence the checked-out commit, which
        // every job records for itself. Failed jobs come first: the commit
        // that matters is the one the failing job tested.
        LogScope::All => {
            let mut jobs = collect_jobs(&view, "jobs", requests, true);
            jobs.extend(collect_jobs(&view, "jobs", requests, false));
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
        .filter(|job| super::run_view::is_unsuccessful(&job["conclusion"]) == unsuccessful)
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
