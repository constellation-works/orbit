use super::super::StreamedLog;
use super::super::log_cache::LogCacheKey;
use super::super::throttle::{check_read_result, throttle_of};
use super::{read::*, selection::*, stream::*};
use orbit_common::security::redaction::redact_all;
use orbit_common::{OrbitError, ProviderReadThrottle};
use orbit_exec::{ExecRequest, NoSandbox, run_process};
use serde_json::{Value, json};
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

/// Why one fallback job read produced no log.
enum JobLogFailure {
    /// Quota admission refused the job-log read before any download.
    Admission(OrbitError),
    /// The read ran, or its metadata check did, and failed.
    Read(OrbitError),
}

/// One verified fallback job's whole log (`request` is its unguarded job-log
/// read, admitted only when the log is actually downloaded), served from the
/// log cache when a current status request shows the job completed for this
/// run under the caller's access and its log is already held for the resolved
/// repository.
///
/// The same rules as [`run_scoped_log`] apply: a hit still needs the status
/// request, a cold read of a completed job renders from the masked copy it
/// stores, and every cache or metadata failure is a plain read. A failed,
/// timed-out, over-limit or empty read never reaches the store. The key names
/// the job log API apart from the run-scoped read's entries for the same job:
/// the bytes differ, and so does the excerpt shape. A cache hit charges no
/// job-log admission, since no job-log request is made.
fn fallback_job_log(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    job: &FallbackJob,
    request: &ExecRequest,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Result<StreamedLog, JobLogFailure> {
    const LABEL: &str = "gh api job logs";
    let shape = ExcerptShape::TailWeighted;
    let selection = requests.selection.clone();
    let admit = || guard(request).map_err(JobLogFailure::Admission);
    let plain = || {
        stream_bounded_log(&admit()?, bounds, shape, LABEL, selection.clone())
            .map_err(JobLogFailure::Read)
    };
    let Some(cache) = &requests.cache else {
        return plain();
    };
    let job_id = job.id.to_string();
    let Some(source) = completed_job_source(requests, &job_id, guard) else {
        return plain();
    };
    let key = LogCacheKey::job_api(&source, &requests.run_id, &job_id);
    if let Some(cached) = cache.get(&key) {
        return Ok(replay_cached_log(bounds, shape, selection, &cached));
    }
    let read = stream_log_source(&admit()?, bounds, shape, LABEL, selection.clone(), true)
        .map_err(JobLogFailure::Read)?;
    Ok(store_completed_log(
        &key, cache, bounds, shape, selection, read,
    ))
}

/// Read per-job logs until one yields text, keeping the empty run-scoped read
/// as the result when none does.
pub(super) fn recover_from_job_logs(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    empty: StreamedLog,
    primary_error: Option<&str>,
    cached_run_view: Option<&Value>,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> RunLogRead {
    let unrecovered = |reason: String,
                       selection_unsupported: bool,
                       throttle: Option<ProviderReadThrottle>| RunLogRead {
        log: empty,
        source: SOURCE_RUN_LOG,
        source_jobs: Vec::new(),
        fallback_error: Some(redact_all(&reason)),
        fallback_throttle: throttle,
        selection_unsupported,
    };

    let qualify = |reason: String| match primary_error {
        Some(primary) => format!("{primary}; job log recovery failed: {reason}"),
        None => reason,
    };
    let jobs = match fallback_jobs(requests, cached_run_view, guard) {
        Ok(jobs) => jobs,
        Err(gap) => {
            return unrecovered(
                qualify(gap.reason),
                false,
                gap.throttle.map(|throttle| *throttle),
            );
        }
    };
    let considered = jobs.len();
    let mut attempts: Vec<String> = Vec::new();
    let mut selection_unsupported = false;
    let mut throttle = None;
    for job in jobs.iter().take(bounds.max_fallback_jobs) {
        match fallback_job_log(
            requests,
            bounds,
            job,
            &requests.job_log_request(job.id),
            guard,
        ) {
            Ok(log) if !log.text.trim().is_empty() => {
                return RunLogRead {
                    log,
                    source: SOURCE_JOB_API_LOG,
                    source_jobs: vec![job.to_json()],
                    fallback_error: None,
                    fallback_throttle: None,
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
            Err(JobLogFailure::Admission(error)) => {
                let throttle = throttle_of(&error).or(throttle);
                return unrecovered(qualify(error.to_string()), selection_unsupported, throttle);
            }
            Err(JobLogFailure::Read(error)) => {
                throttle = throttle.or_else(|| throttle_of(&error));
                attempts.push(format!("{}: {error}", job.label()));
            }
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
    unrecovered(qualify(reason), selection_unsupported, throttle)
}

/// The jobs whose logs may be read for this request, from verified run
/// metadata.
///
/// Every rejection here is deliberate: unverifiable metadata must end the
/// fallback rather than widen it. The failure is returned as prose because it
/// is evidence about *this run's* collection, not a fault the caller can
/// retry differently, except that a provider throttle behind it stays typed.
///
/// `cached_run_view` skips the self-query below entirely when the caller
/// already holds an equivalent, freshly projected payload — identity is still
/// verified against it exactly as it would be against a freshly fetched one.
fn fallback_jobs(
    requests: &RunLogRequests,
    cached_run_view: Option<&Value>,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Result<Vec<FallbackJob>, FallbackGap> {
    let owned;
    let view = match cached_run_view {
        Some(view) => view,
        None => {
            let request = guard(&requests.run_view).map_err(|error| FallbackGap {
                reason: error.to_string(),
                throttle: throttle_of(&error).map(Box::new),
            })?;
            let deadline =
                crate::github_quota::gh_request_deadline(&request, std::time::Instant::now());
            let result = run_process(&request, &NoSandbox)
                .map_err(|error| format!("gh run view could not run: {error}"))?;
            crate::github_quota::observe_gh_result_by_deadline(&request, &result, deadline);
            check_read_result(&result, "gh run view").map_err(|error| FallbackGap {
                reason: error.to_string(),
                throttle: throttle_of(&error).map(Box::new),
            })?;
            owned = super::super::parse_gh_json(&result.stdout, "gh run view")
                .map(|parsed| super::super::run::view::project_run_view(&parsed))
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
            )
            .into());
        }
        None => {
            return Err(format!(
                "gh run view reported no run id for run {}; refusing its job metadata",
                requests.run_id
            )
            .into());
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
        return Err(FallbackGap::from(match (&requests.job, requests.scope) {
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
        }));
    }
    Ok(jobs)
}

/// Why the fallback found no job to read, already rendered, and the provider
/// throttle behind it when one answered.
struct FallbackGap {
    reason: String,
    throttle: Option<Box<ProviderReadThrottle>>,
}

impl From<String> for FallbackGap {
    fn from(reason: String) -> Self {
        Self {
            reason,
            throttle: None,
        }
    }
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
        .filter(|job| super::super::run::view::is_unsuccessful(&job["conclusion"]) == unsuccessful)
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
