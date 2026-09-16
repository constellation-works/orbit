//! Job-bound diagnostic and checkout evidence for current CI failures.

use serde_json::{Value, json};

use super::collect::{
    Bounds, investigation_slots, job_is_cancelled_without_failed_steps, push_retryable_error,
    run_is_completed,
};
use super::query::{CiQueries, LogScope};

/// Inspect each failed job independently. The row keeps run freshness metadata,
/// but all diagnostic and checkout fields belong only to its named job.
pub(super) fn investigate<Q: CiQueries + ?Sized>(
    queries: &Q,
    failure: &Value,
    bounds: &Bounds,
    checkout_log_reads: &mut usize,
    job_log_reads: &mut usize,
    retryable_errors: &mut Vec<Value>,
) -> Vec<Value> {
    let Some(run_id) = failure.get("run_id").and_then(Value::as_u64) else {
        push_retryable_error(
            retryable_errors,
            "registration",
            "run_identity",
            None,
            "current failure has no numeric run_id",
        );
        return vec![failure.clone()];
    };
    let view = match queries.run_view(&run_id.to_string()) {
        Ok(view) => view,
        Err(error) => {
            push_retryable_error(
                retryable_errors,
                "investigation",
                "run_view",
                failure.get("run_id"),
                &error.to_string(),
            );
            return vec![failure.clone()];
        }
    };
    let jobs = view
        .get("failed_jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if jobs.is_empty() {
        if run_is_completed(failure)
            && failure.get("conclusion").and_then(Value::as_str) == Some("cancelled")
        {
            return vec![inconclusive_cancellation_finding(failure, None)];
        }
        if run_is_completed(failure) {
            push_retryable_error(
                retryable_errors,
                "registration",
                "run_view",
                failure.get("run_id"),
                "failed run returned no failed jobs",
            );
        }
        return vec![failure.clone()];
    }

    // Stable numeric identity makes a provider's job ordering irrelevant to
    // which findings receive the bounded reads on repeated sweeps.
    let mut jobs = jobs;
    jobs.sort_by_key(|job| job.get("job_id").and_then(Value::as_u64));
    let mut findings = Vec::new();
    let mut actionable = Vec::new();
    for job in jobs {
        if job_is_cancelled_without_failed_steps(&job) {
            findings.push(inconclusive_cancellation_finding(failure, Some(&job)));
        } else {
            actionable.push(job);
        }
    }
    if actionable.is_empty() {
        return findings;
    }

    let remaining_reads = bounds.max_job_log_reads.saturating_sub(*job_log_reads);
    let selected = if remaining_reads == 1 {
        // Runs reserve a single slot for integration priority. Within a run,
        // equally relevant failed jobs must rotate even with only one read.
        std::collections::BTreeSet::from([
            (bounds.investigation_cursor % actionable.len() as u64) as usize
        ])
    } else {
        investigation_slots(
            actionable.len(),
            remaining_reads,
            bounds.investigation_cursor,
        )
    };
    for (index, job) in actionable.into_iter().enumerate() {
        let mut finding = failure.clone();
        finding["job_id"] = job.get("job_id").cloned().unwrap_or(Value::Null);
        finding["failed_jobs"] = json!([job]);
        let mut errors = Vec::new();
        if !selected.contains(&index) {
            push_retryable_error(
                &mut errors,
                "investigation",
                "job_log_budget",
                failure.get("run_id"),
                "job evidence was not collected because max_job_log_reads was exhausted",
            );
        } else if let Some(job_id) = finding["job_id"].as_u64() {
            *job_log_reads += 1;
            investigate_job(
                queries,
                &mut finding,
                job_id,
                bounds,
                &view,
                checkout_log_reads,
                &mut errors,
            );
        } else {
            push_retryable_error(
                &mut errors,
                "registration",
                "job_identity",
                failure.get("run_id"),
                "failed job has no numeric job_id",
            );
        }
        for error in &mut errors {
            error["job_id"] = finding["job_id"].clone();
        }
        finding["investigated"] = json!(errors.is_empty());
        finding["evidence_state"] = json!(if errors.is_empty() {
            "complete"
        } else {
            "deferred"
        });
        retryable_errors.extend(errors);
        findings.push(finding);
    }
    findings
}

fn inconclusive_cancellation_finding(failure: &Value, job: Option<&Value>) -> Value {
    let mut finding = failure.clone();
    if let Some(job) = job {
        finding["job_id"] = job.get("job_id").cloned().unwrap_or(Value::Null);
        finding["failed_jobs"] = json!([job]);
    }
    finding["investigated"] = json!(true);
    finding["evidence_state"] = json!("inconclusive");
    finding["inconclusive_reason"] = json!("cancelled_without_failed_steps");
    finding
}

fn investigate_job<Q: CiQueries + ?Sized>(
    queries: &Q,
    failure: &mut Value,
    job_id: u64,
    bounds: &Bounds,
    view: &Value,
    checkout_log_reads: &mut usize,
    retryable_errors: &mut Vec<Value>,
) {
    let run_id = failure["run_id"].to_string();
    // The checkout step normally succeeds, so it is absent from a
    // failed-step-only log; only an all-scope read can evidence the commit
    // under test. Reading `All` up front — instead of `Failed` first and a
    // separate `All` read only when checkout turns out to be missing — costs
    // one log read per job instead of two: the same `StreamedLogCollector`
    // pass that finds the failing command also finds the checkout step, so
    // nothing about the failed-step excerpt is lost by asking for more.
    // `checkout_log_reads` still bounds how many of those (larger) all-scope
    // reads a sweep may spend; a job past that budget falls back to the
    // cheaper failed-step-only scope and skips the checkout attempt.
    let attempt_all_scope = *checkout_log_reads < bounds.max_checkout_log_reads;
    let scope = if attempt_all_scope {
        LogScope::All
    } else {
        LogScope::Failed
    };
    let result = queries.run_logs(&run_id, job_id, scope, bounds.log_max_bytes, Some(view));
    // A read recovered via the per-job log API fetches the whole job log
    // regardless of the scope it was asked for, so it never needed the
    // (larger, budgeted) all-scope download to also carry checkout evidence.
    if attempt_all_scope
        && !matches!(&result, Ok(log) if log.source == orbit_tools::github_cli::SOURCE_JOB_API_LOG)
    {
        *checkout_log_reads += 1;
    }
    let log = match result {
        Ok(log) => log,
        Err(error) => {
            push_retryable_error(
                retryable_errors,
                "investigation",
                "run_logs",
                failure.get("run_id"),
                &error.to_string(),
            );
            return;
        }
    };
    if !log_belongs_to_job(&log, job_id) {
        push_retryable_error(
            retryable_errors,
            "registration",
            "log_job_identity",
            failure.get("run_id"),
            "log source does not belong to the requested failed job",
        );
        return;
    }
    let job_api_log_recovered = log.source == orbit_tools::github_cli::SOURCE_JOB_API_LOG;
    failure["log_job_id"] = json!(job_id);
    let diagnostic = bound_diagnostic(&log, failure, job_id);
    if !log.source_complete || (log.truncated && diagnostic.is_none()) {
        push_retryable_error(
            retryable_errors,
            "investigation",
            "job_log_truncated",
            failure.get("run_id"),
            "job log source or display is incomplete and no actionable bound diagnostic evidence is available",
        );
    }
    failure["log_source_complete"] = json!(log.source_complete);
    failure["diagnostic_unit"] = diagnostic.unwrap_or(Value::Null);
    failure["log_excerpt"] = json!(log.text);
    failure["log_truncated"] = json!(log.truncated);
    failure["log_total_bytes"] = json!(log.total_bytes);
    failure["log_returned_bytes"] = json!(log.returned_bytes);
    failure["log_scope"] = json!(scope.as_str());
    failure["log_source"] = json!(log.source);
    failure["log_source_jobs"] = json!(log.source_jobs);
    failure["actual_checkout_shas"] = json!(log.checkout_commits);
    failure["checkout_evidence"] = json!(log.checkout_evidence);
    // A read that ends with no text at all is not a captured excerpt, and the
    // per-job fallback has already had its turn. Record why, so the filed
    // task can say what is missing and the sweep never reads silence as a
    // clean run.
    if log.text.trim().is_empty() {
        push_retryable_error(
            retryable_errors,
            "investigation",
            "run_logs",
            failure.get("run_id"),
            &with_fallback_cause("query returned no failed-step log text", &log),
        );
    }

    let checkout_missing = log.checkout_commits.is_empty() || !log.checkout_evidence_complete;
    if job_api_log_recovered {
        failure["checkout_evidence_scope"] = json!("job_api_log");
        set_checkout_identity(failure, "job_api_log", &log);
        if checkout_missing && retryable_errors.is_empty() {
            push_retryable_error(
                retryable_errors,
                "registration",
                "checkout_evidence",
                failure.get("run_id"),
                "the complete recovered job log contained no actual checkout SHA; the same job remains eligible for a later sweep",
            );
        }
    } else if attempt_all_scope {
        failure["checkout_evidence_scope"] = json!("all");
        set_checkout_identity(failure, "all", &log);
        // A genuinely incomplete scan (the source-byte cap, or a dropped
        // overlong line that could have carried checkout identity) stays
        // fail-closed even when one SHA was already found: it cannot rule
        // out a later, conflicting identity past whatever it didn't manage to
        // read. Only a *display* cap (evidence line/commit count, or an
        // overlong line unrelated to checkout) is exempt — that never
        // touches `checkout_evidence_complete`.
        if !log.checkout_evidence_complete {
            push_retryable_error(
                retryable_errors,
                "registration",
                "checkout_evidence",
                failure.get("run_id"),
                "checkout evidence scan reached its hard limit; actual checkout identity is incomplete",
            );
        } else if log.checkout_commits.is_empty() {
            push_retryable_error(
                retryable_errors,
                "registration",
                "checkout_evidence",
                failure.get("run_id"),
                &with_fallback_cause("run logs contained no actual checkout SHA", &log),
            );
        }
    } else {
        // Past the checkout budget: this was a failed-step-only read, so
        // report real (if any) checkout findings from it, and only flag the
        // skipped attempt when checkout is still missing.
        set_checkout_identity(failure, "failed", &log);
        if checkout_missing {
            failure["checkout_evidence_scope"] = json!("skipped_budget_exhausted");
            push_retryable_error(
                retryable_errors,
                "investigation",
                "checkout_evidence_budget",
                failure.get("run_id"),
                "actual checkout SHA was not collected because max_checkout_log_reads was exhausted",
            );
        } else {
            failure["checkout_evidence_scope"] = json!("failed");
        }
    }
}

/// The primary read is explicitly narrowed with --job. A fallback must name
/// exactly that job; it may never lend another job's checkout or diagnostic.
fn log_belongs_to_job(log: &super::query::RunLog, job_id: u64) -> bool {
    if log.source == orbit_tools::github_cli::SOURCE_RUN_LOG {
        return log.source_jobs.is_empty();
    }
    log.source == orbit_tools::github_cli::SOURCE_JOB_API_LOG
        && log.source_jobs.len() == 1
        && log.source_jobs[0].get("job_id").and_then(Value::as_u64) == Some(job_id)
}

/// An evidence gap, extended with the fallback's own outcome when there was
/// one.
///
/// An empty log read is not proof that a run's logs expired: it is also how
/// the `gh run view --log*` blind spot presents, and the per-job fallback runs
/// precisely then. Whichever way the gap arose, the reader is told which query
/// fell short rather than being left to assume retention.
fn with_fallback_cause(gap: &str, log: &super::query::RunLog) -> String {
    match &log.fallback_error {
        Some(reason) => format!("{gap}; the per-job log fallback recovered none either: {reason}"),
        None => gap.to_string(),
    }
}

fn set_checkout_identity(failure: &mut Value, scope: &str, log: &super::query::RunLog) {
    let state = if !log.checkout_evidence_complete {
        "incomplete"
    } else {
        match log.checkout_commits.len() {
            0 => "missing",
            1 => "observed",
            _ => "ambiguous",
        }
    };
    failure["checkout_evidence_complete"] = json!(log.checkout_evidence_complete);
    failure["checkout_evidence_scanned_bytes"] = json!(log.checkout_evidence_scanned_bytes);
    failure["checkout_evidence_source_truncated"] = json!(log.checkout_evidence_source_truncated);
    failure["checkout_evidence_display_truncated"] = json!(log.checkout_evidence_display_truncated);
    failure["checkout_identity"] = json!({
        "state": state,
        "observed_shas": log.checkout_commits,
        "provenance": {
            "source": "runner_log",
            "job_id": failure.get("job_id"),
            // Which query the runner log was read through, and the job whose
            // own log supplied it when the run-scoped read returned nothing.
            "read_via": log.source,
            "jobs": log.source_jobs,
            "scope": scope,
            "complete": log.checkout_evidence_complete,
            "scanned_bytes": log.checkout_evidence_scanned_bytes,
            "source_truncated": log.checkout_evidence_source_truncated,
            // Display caps (evidence line/commit count, or an unrelated
            // overlong line) reduce what is reported without bearing on
            // whether identity itself was captured; see `complete` for that.
            "display_truncated": log.checkout_evidence_display_truncated,
        },
    });
}

/// A unique runner failure unit can only name a unique failed step. Primary
/// gh output also carries job/step columns; reject conflicting labels rather
/// than borrowing a different step's command. Raw fallback logs have no columns.
fn bound_diagnostic(log: &super::query::RunLog, failure: &Value, job_id: u64) -> Option<Value> {
    if !log.source_complete {
        return None;
    }
    let mut unit = if let Some(text) = &log.diagnostic {
        json!({"kind": "runner_command", "complete": true, "text": text, "returned_bytes": text.len()})
    } else {
        log.failure_regions.clone()?
    };
    let text = unit["text"].as_str()?;
    let job = failure["failed_jobs"].as_array()?.first()?;
    let steps = job["failed_steps"].as_array()?;
    if steps.len() != 1 {
        return None;
    }
    let step = steps[0]["name"]
        .as_str()
        .filter(|name| !name.trim().is_empty())?;
    if log.source == orbit_tools::github_cli::SOURCE_RUN_LOG {
        let job_name = job["name"].as_str()?;
        for line in text.lines() {
            let mut columns = line.splitn(3, '\t');
            if columns.next()? != job_name || columns.next()? != step || columns.next().is_none() {
                return None;
            }
        }
    }
    unit["job_id"] = json!(job_id);
    unit["step"] = json!(step);
    Some(unit)
}
