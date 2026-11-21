use super::super::log_cache::{LogCache, LogCacheKey, RedactedLog};
use super::super::throttle::check_read_result;
use super::super::{LogSelection, StreamedLog, StreamedLogCollector};
use super::selection::*;
use orbit_common::OrbitError;
use orbit_exec::{ExecRequest, NoSandbox, run_process, run_process_streaming_stdout};
use serde_json::Value;
/// The collector one read of `shape` feeds its source bytes into.
fn log_collector(
    bounds: LogReadBounds,
    shape: ExcerptShape,
    selection: Option<LogSelection>,
) -> StreamedLogCollector {
    let LogReadBounds {
        max_bytes,
        max_evidence_lines,
        ..
    } = bounds;
    match (shape, selection) {
        (ExcerptShape::EvenSplit, Some(selection)) => {
            StreamedLogCollector::scoped(max_bytes, max_evidence_lines, selection)
        }
        (ExcerptShape::TailWeighted, Some(selection)) => {
            StreamedLogCollector::tail_weighted_scoped(max_bytes, max_evidence_lines, selection)
        }
        (ExcerptShape::EvenSplit, None) => StreamedLogCollector::new(max_bytes, max_evidence_lines),
        (ExcerptShape::TailWeighted, None) => {
            StreamedLogCollector::tail_weighted(max_bytes, max_evidence_lines)
        }
    }
}

/// Read one `gh` stdout stream up to the source cap, retaining only bounded
/// display, command, and checkout evidence. No unbounded source copy is held.
pub(super) fn stream_bounded_log(
    request: &ExecRequest,
    bounds: LogReadBounds,
    shape: ExcerptShape,
    label: &str,
    selection: Option<LogSelection>,
) -> Result<StreamedLog, OrbitError> {
    stream_log_source(request, bounds, shape, label, selection, false).map(|(log, _)| log)
}

/// [`stream_bounded_log`], also returning the source bytes it read when
/// `keep_source` is set. The copy is bounded by the same source read limit, and
/// exists only for a read the caller is about to cache.
pub(super) fn stream_log_source(
    request: &ExecRequest,
    bounds: LogReadBounds,
    shape: ExcerptShape,
    label: &str,
    selection: Option<LogSelection>,
    keep_source: bool,
) -> Result<(StreamedLog, Vec<u8>), OrbitError> {
    let deadline = crate::github_quota::gh_request_deadline(request, std::time::Instant::now());
    let limit_error =
        format!("{label} exceeded the 8 MiB source read limit; evidence is incomplete");
    let (result, read) = run_process_streaming_stdout(request, &NoSandbox, move |mut stdout| {
        use std::io::Read;

        let mut collector = log_collector(bounds, shape, selection);
        let mut source = Vec::new();
        let mut chunk = [0_u8; 4096];
        let mut source_bytes = 0usize;
        loop {
            let read = stdout.read(&mut chunk).map_err(|error| {
                OrbitError::Execution(format!("failed reading gh log stream: {error}"))
            })?;
            if read == 0 {
                return Ok((collector.finish(), source));
            }
            source_bytes += read;
            if source_bytes > super::super::MAX_CHECKOUT_LOG_SCAN_BYTES {
                return Err(OrbitError::Execution(limit_error));
            }
            collector.push(&chunk[..read]);
            if keep_source {
                source.extend_from_slice(&chunk[..read]);
            }
        }
    })?;
    crate::github_quota::observe_gh_result_by_deadline(request, &result, deadline);
    // The supervisor's verdict, not the stderr note it appends, says the read
    // was killed; callers bounding the read by a deadline match this variant.
    if result.timed_out {
        return Err(OrbitError::ProcessTimeout {
            timeout_ms: request.timeout_ms.unwrap_or_default(),
            detail: format!("{label}: {}", result.stderr.trim()),
        });
    }
    check_read_result(&result, label)?;
    Ok(read)
}

/// Render a cached masked log through the collector one source read of `shape`
/// feeds, so a warm read returns what the cold read that filled the cache did.
pub(super) fn replay_cached_log(
    bounds: LogReadBounds,
    shape: ExcerptShape,
    selection: Option<LogSelection>,
    log: &[u8],
) -> StreamedLog {
    let mut collector = log_collector(bounds, shape, selection);
    for piece in log.chunks(4096) {
        collector.push(piece);
    }
    collector.finish()
}

/// Mask a freshly read completed log, store it, and answer from the stored
/// form so the first read matches every later one. A log that cannot be
/// masked is not stored and answers from the read itself.
pub(super) fn store_completed_log(
    key: &LogCacheKey,
    cache: &LogCache,
    bounds: LogReadBounds,
    shape: ExcerptShape,
    selection: Option<LogSelection>,
    (log, source): (StreamedLog, Vec<u8>),
) -> StreamedLog {
    let Some(masked) = RedactedLog::from_source(&source) else {
        return log;
    };
    cache.put(key, &masked);
    if masked.as_bytes() == source.as_slice() {
        return log;
    }
    replay_cached_log(bounds, shape, selection, masked.as_bytes())
}

/// The run-scoped `gh run view --log*` read, served from the log cache when
/// a current status request shows the job or run it names completed under the
/// caller's access and its log is already held for the resolved repository.
///
/// The cache holds a masked copy of the source (see `log_cache.rs`) with the
/// source's lengths, and every read of a cacheable log, the one that fills the
/// cache included, replays that copy through the collector, so a cold and a
/// warm read return the same result in source coordinates. Every cache or
/// metadata failure falls back to the plain read: the cache can make a read
/// cheaper, never fail it.
pub(super) fn run_scoped_log(
    requests: &RunLogRequests,
    bounds: LogReadBounds,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Result<StreamedLog, OrbitError> {
    const LABEL: &str = "gh run view --log";
    let read_from =
        |request: &ExecRequest, keep_source: bool| -> Result<(StreamedLog, Vec<u8>), OrbitError> {
            stream_log_source(
                &guard(request)?,
                bounds,
                ExcerptShape::EvenSplit,
                LABEL,
                requests.selection.clone(),
                keep_source,
            )
        };
    let read = |keep_source: bool| read_from(&requests.run_log, keep_source);
    let replay = |log: &[u8]| {
        replay_cached_log(
            bounds,
            ExcerptShape::EvenSplit,
            requests.selection.clone(),
            log,
        )
    };
    let store = |key: &LogCacheKey, cache: &LogCache, fetched: (StreamedLog, Vec<u8>)| {
        store_completed_log(
            key,
            cache,
            bounds,
            ExcerptShape::EvenSplit,
            requests.selection.clone(),
            fetched,
        )
    };
    let Some(cache) = &requests.cache else {
        return read(false).map(|(log, _)| log);
    };
    let scope = requests.scope.as_str();

    // A completed job id names one immutable log, so a hit needs no download.
    // It still needs the status request: that request runs under the current
    // credential and checkout, so it is what proves this caller may read the
    // job today, and what resolves the host and repository the entry belongs
    // to (the request's own `{owner}/{repo}` placeholders resolve to whatever
    // the checkout points at). A refused or unreadable status request is a
    // plain read, which reports the provider's own error.
    if let Some(job) = &requests.job {
        let Some(source) = completed_job_source(requests, job, guard) else {
            return read(false).map(|(log, _)| log);
        };
        let key = LogCacheKey::job(&source, &requests.run_id, job, scope);
        if let Some(cached) = cache.get(&key) {
            return Ok(replay(&cached));
        }
        return Ok(store(&key, cache, read(true)?));
    }

    // A rerun keeps the run id and puts the run back in progress, so each read
    // first asks which attempt the run is at and whether it has completed.
    let Some((source, attempt)) = completed_run_attempt(requests, guard) else {
        return read(false).map(|(log, _)| log);
    };
    let key = LogCacheKey::run_attempt(&source, &requests.run_id, attempt, scope);
    if let Some(cached) = cache.get(&key) {
        return Ok(replay(&cached));
    }
    // Bind the download to `attempt` and re-verify the completed attempt
    // afterwards before caching, so a rerun between metadata and download
    // never writes another attempt's bytes under `key`.
    let attempt_request = attempt_bound_run_log_request(&requests.run_log, attempt);
    let (log, downloaded) = read_from(&attempt_request, true)?;
    if downloaded.is_empty()
        || completed_run_attempt(requests, guard).as_ref() != Some(&(source, attempt))
    {
        return Ok(log);
    }
    Ok(store(&key, cache, (log, downloaded)))
}

/// One lightweight metadata read, or `None` when it cannot be made or parsed.
fn read_api_json(
    request: ExecRequest,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Option<Value> {
    let request = guard(&request).ok()?;
    let deadline = crate::github_quota::gh_request_deadline(&request, std::time::Instant::now());
    let result = run_process(&request, &NoSandbox).ok()?;
    crate::github_quota::observe_gh_result_by_deadline(&request, &result, deadline);
    if !result.success {
        return None;
    }
    serde_json::from_str(&result.stdout).ok()
}

/// The resolved source of a completed job of this run: the host and repository
/// GitHub itself reports for the job (its `url`, minus the job endpoint), or
/// `None` when the job is not reported completed, is not this run's job, or
/// the status request failed or named no source.
pub(super) fn completed_job_source(
    requests: &RunLogRequests,
    job: &str,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Option<String> {
    let request = requests.api_request(format!("repos/{}/actions/jobs/{job}", requests.repository));
    let view = read_api_json(request, guard)?;
    let this_job = view.get("id").and_then(Value::as_u64)?.to_string() == job;
    let this_run = view
        .get("run_id")
        .and_then(Value::as_u64)
        .is_some_and(|run| run.to_string() == requests.run_id);
    let completed = view.get("status").and_then(Value::as_str) == Some("completed");
    if !(this_job && this_run && completed) {
        return None;
    }
    resolved_source(&view, &format!("/actions/jobs/{job}"))
}

/// The resolved source and attempt number of this run when GitHub reports it
/// completed.
fn completed_run_attempt(
    requests: &RunLogRequests,
    guard: &dyn Fn(&ExecRequest) -> Result<ExecRequest, OrbitError>,
) -> Option<(String, u64)> {
    let request = requests.api_request(format!(
        "repos/{}/actions/runs/{}",
        requests.repository, requests.run_id
    ));
    let view = read_api_json(request, guard)?;
    let this_run = view.get("id").and_then(Value::as_u64)?.to_string() == requests.run_id;
    let completed = view.get("status").and_then(Value::as_str) == Some("completed");
    if !(this_run && completed) {
        return None;
    }
    let source = resolved_source(&view, &format!("/actions/runs/{}", requests.run_id))?;
    Some((source, view.get("run_attempt").and_then(Value::as_u64)?))
}

/// The host and repository a status payload names: its API `url` with the
/// `endpoint` it must end in removed (`https://api.github.com/repos/o/n`).
/// `None` when the payload carries no such URL, so an entry is never filed
/// under a source nobody resolved.
fn resolved_source(view: &Value, endpoint: &str) -> Option<String> {
    let url = view.get("url").and_then(Value::as_str)?;
    let source = url.strip_suffix(endpoint)?;
    (source.starts_with("https://") || source.starts_with("http://")).then(|| source.to_string())
}
