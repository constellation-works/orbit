//! Manual isolated public-boundary fixture. Pass the evaluator schema JSON.
//! No live HTTP or promotional credits; no automated feature tests are added.
mod fixture;
use chrono::Duration;
use fixture::{
    AssistanceFixture, CREDENTIAL, FixtureResult, SUPPLIED, SecondAssistanceAdapter,
    ambiguous_non_timeout_send_failure_reason, builder_send_failure_reason,
    connect_send_failure_reason, timeout_send_failure_reason,
};
use orbit_common::OrbitError;
use orbit_core::adapter::decisions::{AssistanceHttpResponse, OPENAI_DECISIONS_ADAPTER_IDENTITY};
use orbit_core::application::assistance::AssistanceAdapter;
use orbit_store::Store;
use orbit_store::contracts::assistance::{AssistanceAdmission, AssistanceStoreBackend};
use orbit_types::assistance::{
    self, AssistanceDisposition, AssistanceMode, AssistanceReason, AssistanceResult,
};
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn fixture_service_boundaries(root: &Path, schema: &Value) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    f.settings
        .lock()
        .map_err(|_| "settings lock")?
        .policies
        .clear();
    assert_eq!(
        f.service.enqueue_assistance(f.request_for_off()?, &[])?,
        AssistanceAdmission::Disabled
    );
    assert!(
        f.service
            .process_next_assistance("ws_fixture", "task_quality")?
            .is_none()
    );
    assert_eq!(f.credential.0.load(Ordering::SeqCst), 0);
    assert!(
        f.transport
            .bodies
            .lock()
            .map_err(|_| "body lock")?
            .is_empty()
    );
    assert_eq!(
        f.store.assistance_accounting("synthetic-only/1")?.attempts,
        0
    );
    assert_eq!(
        f.store.with_read_connection(|conn| conn
            .query_row("SELECT COUNT(*) FROM assistance_work", [], |r| r
                .get::<_, u32>(0))
            .map_err(|_| orbit_common::OrbitError::Store("fixture queue read".into())))?,
        0
    );
    // Recreate the fixture in another isolated store for enabled behavior.
    let f = AssistanceFixture::open_fixture(&root.join("enabled"), schema, "task_quality")?;
    let request = f.request("first")?;
    let source = request.source_digest.clone();
    let id = f.enqueue(request.clone())?;
    assert_eq!(f.enqueue(request)?, id);
    f.respond(f.good_response())?;
    f.process()?;
    let record = f.record(&id)?;
    let encoded = serde_json::to_string(&record)?;
    assert!(!encoded.contains(CREDENTIAL) && !encoded.contains("fixture-secret"));
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(
        record.result.as_ref().ok_or("missing result")?.disposition,
        AssistanceDisposition::Suggested
    );
    assert!(f.service.assistance_advisory(&id, &source)?.is_some());
    assert!(
        f.service
            .assistance_advisory(&id, &assistance::assistance_digest(b"edited"))?
            .is_none()
    );
    assert!(!f.store.assistance_finish(
        &orbit_store::contracts::assistance::AssistanceClaim {
            work: record.work.clone(),
            attempt_id: record.attempts[0].attempt_id.clone(),
            ordinal: 1,
            deadline: f.now()? + Duration::seconds(15)
        },
        record.result.clone().ok_or("missing result")?,
        f.now()?
    )?);
    // Host scope strips ungranted data and preserves omissions; no fetching.
    let mut scoped = f.request("scope")?;
    scoped
        .evidence_packet
        .fields
        .insert("ambient_environment".into(), json!("never outbound"));
    scoped
        .evidence_packet
        .evidence_refs
        .push("https://unpermitted.invalid/attachment".into());
    let scoped_id = f.enqueue(scoped)?;
    f.respond(f.good_response())?;
    f.process()?;
    let record = f.record(&scoped_id)?;
    assert!(record.work.request.evidence_packet.truncated);
    assert_eq!(
        record.result.ok_or("missing scoped result")?.disposition,
        AssistanceDisposition::Uncertain
    );
    assert!(
        !record
            .work
            .request
            .evidence_packet
            .fields
            .contains_key("ambient_environment")
    );
    assert_eq!(
        record.work.request.evidence_packet.evidence_refs,
        vec!["fixture:permitted"]
    );
    // Off after claim: a late response cannot become advisory.
    let late = f.enqueue(f.request("switch-off")?)?;
    let settings = f.settings.clone();
    *f.transport.on_send.lock().map_err(|_| "hook lock")? = Some(Box::new(move || {
        if let Ok(mut s) = settings.lock() {
            s.policies[0].mode = AssistanceMode::Off;
        }
    }));
    f.respond(f.good_response())?;
    f.process()?;
    assert_eq!(
        f.record(&late)?
            .result
            .ok_or("missing stale result")?
            .disposition,
        AssistanceDisposition::Stale
    );
    Ok(())
}

fn fixture_completed_result_outlives_queue_age(root: &Path, schema: &Value) -> FixtureResult {
    // A completed result is not queued work: queue age must not expire it or
    // relabel it Superseded. It stays readable until retention maintenance
    // removes it or a real supersession (digest change, fresher snapshot)
    // replaces it.
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    let request = f.request("completed")?;
    let source = request.source_digest.clone();
    let id = f.enqueue(request.clone())?;
    f.respond(f.good_response())?;
    f.process()?;
    let limits = f.record(&id)?.work.limits;
    assert_eq!(
        f.record(&id)?
            .result
            .as_ref()
            .ok_or("missing completed result")?
            .disposition,
        AssistanceDisposition::Suggested
    );
    f.advance(i64::from(limits.queue_age_seconds) + 1)?;
    let advisory = f
        .service
        .assistance_advisory(&id, &source)?
        .ok_or("completed result expired at queue age")?;
    assert_eq!(advisory.disposition, AssistanceDisposition::Suggested);
    assert!(!advisory.answers.is_empty());
    let assessment = f
        .service
        .assistance_assessment(&id, &source)?
        .ok_or("missing assessment after queue age")?;
    assert!(assessment.current);
    assert_eq!(
        assessment
            .result
            .as_ref()
            .ok_or("missing result after queue age")?
            .disposition,
        AssistanceDisposition::Suggested
    );
    // A changed source digest is real supersession and still hides it.
    assert!(
        f.service
            .assistance_advisory(&id, &assistance::assistance_digest(b"edited"))?
            .is_none()
    );
    let superseded_by_digest = f
        .service
        .assistance_assessment(&id, &assistance::assistance_digest(b"edited"))?
        .ok_or("missing digest assessment")?;
    assert!(!superseded_by_digest.current);
    assert_eq!(
        superseded_by_digest
            .result
            .as_ref()
            .ok_or("missing superseded result")?
            .reason,
        AssistanceReason::Superseded
    );
    // A fresher snapshot for the same subject also supersedes it.
    let mut newer = request;
    newer.source_digest = assistance::assistance_digest(b"new semantic revision");
    newer.evidence_packet.collected_at = f.now()?;
    let new_id = f.enqueue(newer)?;
    assert_ne!(id, new_id);
    assert!(f.service.assistance_advisory(&id, &source)?.is_none());
    let superseded_by_snapshot = f
        .service
        .assistance_assessment(&id, &source)?
        .ok_or("missing snapshot assessment")?;
    assert!(!superseded_by_snapshot.current);
    assert_eq!(
        superseded_by_snapshot
            .result
            .as_ref()
            .ok_or("missing snapshot-superseded result")?
            .reason,
        AssistanceReason::Superseded
    );
    // Retention ends at created + queue age + retention; maintenance removes it.
    f.advance(i64::from(limits.retention_seconds))?;
    f.store.assistance_maintain(f.now()?)?;
    assert!(f.store.assistance_record(&id)?.is_none());
    assert!(f.service.assistance_advisory(&id, &source)?.is_none());
    Ok(())
}

fn fixture_expired_queued_work_reads_match_maintenance(
    root: &Path,
    schema: &Value,
) -> FixtureResult {
    // Queue age expires queued work. The current flag and advisory must not
    // depend on whether maintenance has yet recorded the QueueExpired gap, and
    // a retry attempt's gap must not read as a current result.
    let queued = AssistanceFixture::open_fixture(&root.join("queued"), schema, "task_quality")?;
    let request = queued.request("expired-queued")?;
    let source = request.source_digest.clone();
    let id = queued.enqueue(request)?;
    let queue_age = i64::from(queued.record(&id)?.work.limits.queue_age_seconds);
    queued.advance(queue_age + 1)?;
    assert!(expired_uncompleted_read(&queued, &id, &source)?.is_none());
    queued.store.assistance_maintain(queued.now()?)?;
    assert_eq!(
        expired_uncompleted_read(&queued, &id, &source)?
            .ok_or("missing queue-expiry gap after maintenance")?
            .reason,
        AssistanceReason::QueueExpired
    );

    let retrying =
        AssistanceFixture::open_fixture(&root.join("retry-pending"), schema, "task_quality")?;
    let request = retrying.request("expired-retry")?;
    let source = request.source_digest.clone();
    let id = retrying.enqueue(request)?;
    let limits = retrying.record(&id)?.work.limits;
    assert!(
        limits.max_attempts > 1,
        "retry-pending case needs a retry budget"
    );
    retrying
        .transport
        .replies
        .lock()
        .map_err(|_| "replies lock")?
        .push_back(Ok(AssistanceHttpResponse {
            status: 503,
            body: br#"{"error":"overloaded"}"#.to_vec(),
        }));
    retrying.process()?;
    assert_eq!(
        retrying
            .record(&id)?
            .result
            .ok_or("missing retry-pending gap")?
            .reason,
        AssistanceReason::Unavailable
    );
    retrying.advance(i64::from(limits.queue_age_seconds) + 1)?;
    assert!(expired_uncompleted_read(&retrying, &id, &source)?.is_none());
    retrying.store.assistance_maintain(retrying.now()?)?;
    assert_eq!(
        expired_uncompleted_read(&retrying, &id, &source)?
            .ok_or("missing retry expiry gap after maintenance")?
            .reason,
        AssistanceReason::QueueExpired
    );
    Ok(())
}

/// Asserts that an expired record without a completed answer is not current
/// and has no advisory, then returns the assessment's result for the caller.
fn expired_uncompleted_read(
    f: &AssistanceFixture,
    id: &str,
    source: &str,
) -> FixtureResult<Option<AssistanceResult>> {
    let assessment = f
        .service
        .assistance_assessment(id, source)?
        .ok_or("missing expired assessment")?;
    assert!(
        !assessment.current,
        "expired uncompleted work reads as current"
    );
    assert!(
        f.service.assistance_advisory(id, source)?.is_none(),
        "expired uncompleted work reads an advisory"
    );
    Ok(assessment.result)
}

fn fixture_send_failure_reconciliation(root: &Path, schema: &Value) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    let id = f.enqueue(f.request("lost-response")?)?;
    let work = f.record(&id)?.work;
    f.fail_send(ambiguous_non_timeout_send_failure_reason())?;
    f.process()?;

    let after_loss = f.record(&id)?;
    let first_attempt = after_loss
        .attempts
        .first()
        .ok_or("lost-response attempt missing")?;
    let accounting = f.store.assistance_accounting("synthetic-only/1")?;
    let reserved_charge = work
        .budget
        .assistance_charge_bound(u64::from(work.limits.max_input_tokens))
        .ok_or("synthetic charge bound missing")?;
    assert!(
        first_attempt.outcome_unknown,
        "synthetic non-timeout lost-response was recorded as known: reason={:?}, unknown_attempts={}, attempt_reason={:?}",
        first_attempt.result.as_ref().map(|result| result.reason),
        accounting.outcome_unknown_attempts,
        first_attempt.result.as_ref().map(|result| result.reason),
    );
    assert!(after_loss.current);
    assert_eq!(
        first_attempt
            .result
            .as_ref()
            .ok_or("lost-response result missing")?
            .reason,
        AssistanceReason::TimeoutUnknown
    );
    assert_eq!(
        first_attempt.reserved_tokens,
        u64::from(work.limits.max_input_tokens)
    );
    assert_eq!(
        accounting.reserved_tokens,
        u64::from(work.limits.max_input_tokens)
    );
    assert_eq!(first_attempt.reserved_charge_microusd, reserved_charge);
    assert_eq!(accounting.reserved_charge_microusd, reserved_charge);
    assert_eq!(accounting.attempts, 1);
    assert_eq!(accounting.outcome_unknown_attempts, 1);
    let first_attempt_id = first_attempt.attempt_id.clone();

    f.advance(i64::from(work.limits.retry_delay_seconds))?;
    f.respond(f.good_response())?;
    f.process()?;
    let after_retry = f.record(&id)?;
    assert!(after_retry.current);
    assert_eq!(after_retry.attempts.len(), 2);
    assert_eq!(after_retry.attempts[0].ordinal, 1);
    assert_eq!(after_retry.attempts[1].ordinal, 2);
    assert_ne!(after_retry.attempts[1].attempt_id, first_attempt_id);
    assert_eq!(
        after_retry
            .result
            .as_ref()
            .ok_or("retry result missing")?
            .disposition,
        AssistanceDisposition::Suggested
    );
    let retry_accounting = f.store.assistance_accounting("synthetic-only/1")?;
    assert_eq!(retry_accounting.attempts, 2);
    assert_eq!(retry_accounting.outcome_unknown_attempts, 1);
    assert_eq!(
        retry_accounting.reserved_tokens,
        u64::from(work.limits.max_input_tokens) * 2
    );
    assert_eq!(
        retry_accounting.reserved_charge_microusd,
        reserved_charge * 2
    );
    f.advance(i64::from(work.limits.retry_delay_seconds))?;
    f.process()?;
    assert_eq!(f.record(&id)?.attempts.len(), 2);
    assert_eq!(
        f.store.assistance_accounting("synthetic-only/1")?.attempts,
        2
    );

    let pre_send = AssistanceFixture::open_fixture(&root.join("pre-send"), schema, "task_quality")?;
    let pre_send_id = pre_send.enqueue(pre_send.request("pre-send-builder")?)?;
    pre_send.fail_send(builder_send_failure_reason())?;
    pre_send.process()?;
    let pre_send_record = pre_send.record(&pre_send_id)?;
    let pre_send_attempt = pre_send_record
        .attempts
        .first()
        .ok_or("pre-send attempt missing")?;
    assert!(!pre_send_attempt.outcome_unknown);
    assert_eq!(
        pre_send_attempt
            .result
            .as_ref()
            .ok_or("pre-send result missing")?
            .reason,
        AssistanceReason::Unavailable
    );
    assert_eq!(
        pre_send
            .store
            .assistance_accounting("synthetic-only/1")?
            .outcome_unknown_attempts,
        0
    );
    // A builder failure enters the transport client, so its reservation is charged.
    assert_eq!(
        pre_send
            .store
            .assistance_accounting("synthetic-only/1")?
            .reserved_tokens,
        u64::from(pre_send_record.work.limits.max_input_tokens)
    );

    let connect = AssistanceFixture::open_fixture(&root.join("connect"), schema, "task_quality")?;
    let connect_id = connect.enqueue(connect.request("connect-failure")?)?;
    connect.fail_send(connect_send_failure_reason())?;
    connect.process()?;
    let connect_attempt = connect
        .record(&connect_id)?
        .attempts
        .into_iter()
        .next()
        .ok_or("connect attempt missing")?;
    assert!(!connect_attempt.outcome_unknown);
    assert_eq!(
        connect_attempt
            .result
            .ok_or("connect result missing")?
            .reason,
        AssistanceReason::Unavailable
    );

    let timeout = AssistanceFixture::open_fixture(&root.join("timeout"), schema, "task_quality")?;
    let timeout_id = timeout.enqueue(timeout.request("timeout-failure")?)?;
    timeout.fail_send(timeout_send_failure_reason())?;
    timeout.process()?;
    let timeout_record = timeout.record(&timeout_id)?;
    let timeout_attempt = timeout_record
        .attempts
        .first()
        .ok_or("timeout attempt missing")?;
    assert!(timeout_attempt.outcome_unknown);
    assert_eq!(
        timeout_attempt
            .result
            .as_ref()
            .ok_or("timeout result missing")?
            .reason,
        AssistanceReason::TimeoutUnknown
    );
    Ok(())
}

fn fixture_http_status_retry(root: &Path, schema: &Value) -> FixtureResult {
    // A revoked credential (401) is definitive: the same request fails again,
    // so it completes after one attempt and one reservation with no retry.
    let f = AssistanceFixture::open_fixture(&root.join("rejected"), schema, "task_quality")?;
    let id = f.enqueue(f.request("revoked-credential")?)?;
    let work = f.record(&id)?.work;
    let charge = work
        .budget
        .assistance_charge_bound(u64::from(work.limits.max_input_tokens))
        .ok_or("synthetic charge bound missing")?;
    for _ in 0..2 {
        f.transport
            .replies
            .lock()
            .map_err(|_| "replies lock")?
            .push_back(Ok(AssistanceHttpResponse {
                status: 401,
                body: br#"{"error":"revoked"}"#.to_vec(),
            }));
    }
    f.process()?;
    f.advance(i64::from(work.limits.retry_delay_seconds) + 1)?;
    f.process()?;
    let record = f.record(&id)?;
    assert_eq!(
        record.attempts.len(),
        1,
        "definitive 401 was retried: attempts={}",
        record.attempts.len()
    );
    assert_eq!(
        record
            .result
            .as_ref()
            .ok_or("missing rejection result")?
            .reason,
        AssistanceReason::Refused
    );
    let accounting = f.store.assistance_accounting("synthetic-only/1")?;
    assert_eq!(accounting.attempts, 1);
    assert_eq!(accounting.outcome_unknown_attempts, 0);
    assert_eq!(
        accounting.reserved_tokens,
        u64::from(work.limits.max_input_tokens)
    );
    assert_eq!(accounting.reserved_charge_microusd, charge);
    assert_eq!(
        f.transport.bodies.lock().map_err(|_| "body lock")?.len(),
        1,
        "definitive 401 reached the provider twice"
    );

    // A 503 may succeed later, so it keeps the bounded retry: two attempts and
    // two reservations, then the good retry completes without a third attempt.
    let g = AssistanceFixture::open_fixture(&root.join("overloaded"), schema, "task_quality")?;
    let id = g.enqueue(g.request("overloaded")?)?;
    let work = g.record(&id)?.work;
    let charge = work
        .budget
        .assistance_charge_bound(u64::from(work.limits.max_input_tokens))
        .ok_or("synthetic charge bound missing")?;
    g.transport
        .replies
        .lock()
        .map_err(|_| "replies lock")?
        .push_back(Ok(AssistanceHttpResponse {
            status: 503,
            body: br#"{"error":"overloaded"}"#.to_vec(),
        }));
    g.process()?;
    let after_first = g.record(&id)?;
    assert_eq!(after_first.attempts.len(), 1);
    assert_eq!(
        after_first
            .result
            .as_ref()
            .ok_or("missing overload result")?
            .reason,
        AssistanceReason::Unavailable
    );
    g.advance(i64::from(work.limits.retry_delay_seconds))?;
    g.respond(g.good_response())?;
    g.process()?;
    let after_retry = g.record(&id)?;
    assert_eq!(after_retry.attempts.len(), 2);
    assert_eq!(after_retry.attempts[0].ordinal, 1);
    assert_eq!(after_retry.attempts[1].ordinal, 2);
    assert_ne!(
        after_retry.attempts[1].attempt_id,
        after_retry.attempts[0].attempt_id
    );
    assert_eq!(
        after_retry
            .result
            .as_ref()
            .ok_or("missing overload retry result")?
            .disposition,
        AssistanceDisposition::Suggested
    );
    let retry_accounting = g.store.assistance_accounting("synthetic-only/1")?;
    assert_eq!(retry_accounting.attempts, 2);
    assert_eq!(retry_accounting.outcome_unknown_attempts, 0);
    assert_eq!(
        retry_accounting.reserved_tokens,
        u64::from(work.limits.max_input_tokens) * 2
    );
    assert_eq!(retry_accounting.reserved_charge_microusd, charge * 2);
    g.advance(i64::from(work.limits.retry_delay_seconds))?;
    g.process()?;
    assert_eq!(g.record(&id)?.attempts.len(), 2);
    assert_eq!(
        g.store.assistance_accounting("synthetic-only/1")?.attempts,
        2
    );
    Ok(())
}

fn fixture_credential_refusal_releases_reservation(root: &Path, schema: &Value) -> FixtureResult {
    // Unavailable credentials refuse locally: no request leaves the host, so no
    // reservation may survive and the allocation must not drain.
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    f.credential.1.store(false, Ordering::SeqCst);
    let mut last = String::new();
    for n in 0..40 {
        last = f.enqueue(f.request(&format!("credential-unavailable-{n}"))?)?;
        f.process()?;
    }
    let accounting = f.store.assistance_accounting("synthetic-only/1")?;
    assert_eq!(
        accounting.reserved_tokens, 0,
        "credential refusals before send kept their token reservation"
    );
    assert_eq!(
        accounting.reserved_charge_microusd, 0,
        "credential refusals before send kept their charge reservation"
    );
    assert_eq!(accounting.attempts, 0);
    assert_eq!(accounting.outcome_unknown_attempts, 0);
    assert!(
        f.transport
            .bodies
            .lock()
            .map_err(|_| "body lock")?
            .is_empty()
    );
    assert_eq!(
        f.record(&last)?
            .result
            .ok_or("missing credential result")?
            .reason,
        AssistanceReason::CredentialUnavailable
    );
    Ok(())
}

fn fixture_settings_read_failure_releases_reservation(
    root: &Path,
    schema: &Value,
) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    let id = f.enqueue(f.request("settings-read-failure")?)?;
    let before = f.store.assistance_accounting("synthetic-only/1")?;
    let credential_reads = f.credential.0.load(Ordering::SeqCst);
    // Processing reads settings before claiming, then rechecks with the claim
    // held. Fail only that second read; the loader recovers on the next read.
    f.settings_reads_until_failure.store(2, Ordering::SeqCst);
    assert!(matches!(
        f.service.process_next_assistance("ws_fixture", "task_quality"),
        Err(OrbitError::InvalidInput(message)) if message == "fixture settings read failure"
    ));
    let record = f.record(&id)?;
    assert_eq!(record.attempts.len(), 1, "settings failed before the claim");
    assert_eq!(f.credential.0.load(Ordering::SeqCst), credential_reads);
    assert!(
        f.transport
            .bodies
            .lock()
            .map_err(|_| "body lock")?
            .is_empty()
    );

    // Maintenance used to expire the abandoned claim as outcome-unknown and
    // keep both reservations. Advance the fixture clock without a wall wait.
    f.advance(i64::from(record.work.limits.timeout_seconds) + 1)?;
    f.store.assistance_maintain(f.now()?)?;
    let after = f.store.assistance_accounting("synthetic-only/1")?;
    assert_eq!(
        after.outcome_unknown_attempts, before.outcome_unknown_attempts,
        "settings read failure became unknown; tokens={}, charge={}",
        after.reserved_tokens, after.reserved_charge_microusd
    );
    assert_eq!(after.reserved_tokens, before.reserved_tokens);
    assert_eq!(
        after.reserved_charge_microusd,
        before.reserved_charge_microusd
    );
    assert_eq!(after.attempts, before.attempts);
    let attempt = &record.attempts[0];
    assert!(!attempt.outcome_unknown);
    assert_eq!(attempt.reserved_tokens, 0);
    assert_eq!(attempt.reserved_charge_microusd, 0);
    assert_eq!(
        record.result.ok_or("missing settings refusal")?.reason,
        AssistanceReason::ConfigurationChanged
    );
    Ok(())
}

fn fixture_deadline_before_send_is_not_unknown(root: &Path, schema: &Value) -> FixtureResult {
    // A deadline reached before send is a local refusal, not an outcome-unknown
    // attempt. The clock passes the claim deadline between claim and deadline check.
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    let id = f.enqueue(f.request("deadline-before-send")?)?;
    let clock = f.clock.clone();
    let advanced = AtomicBool::new(false);
    f.service
        .process_next_assistance_checked("ws_fixture", "task_quality", |_| {
            if !advanced.swap(true, Ordering::SeqCst) {
                *clock
                    .lock()
                    .map_err(|_| OrbitError::InvalidInput("fixture clock lock".into()))? +=
                    Duration::seconds(60);
            }
            Ok(true)
        })?;
    let record = f.record(&id)?;
    let attempt = record.attempts.first().ok_or("deadline attempt missing")?;
    assert!(
        !attempt.outcome_unknown,
        "a deadline reached before send was recorded as an outcome-unknown attempt"
    );
    assert_eq!(
        record
            .result
            .as_ref()
            .ok_or("deadline result missing")?
            .disposition,
        AssistanceDisposition::Unavailable
    );
    let accounting = f.store.assistance_accounting("synthetic-only/1")?;
    assert_eq!(accounting.outcome_unknown_attempts, 0);
    assert_eq!(accounting.reserved_tokens, 0);
    assert_eq!(accounting.reserved_charge_microusd, 0);
    assert_eq!(accounting.attempts, 0);
    assert!(
        f.transport
            .bodies
            .lock()
            .map_err(|_| "body lock")?
            .is_empty()
    );
    Ok(())
}

fn fixture_restart_retry(root: &Path, schema: &Value) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    *f.clock.lock().map_err(|_| "clock lock")? += Duration::microseconds(123);
    f.settings
        .lock()
        .map_err(|_| "settings lock")?
        .limits
        .concurrency = 1;
    let id = f.enqueue(f.request("restart")?)?;
    let work = f.record(&id)?.work;
    let claim = f
        .store
        .assistance_claim("ws_fixture", "task_quality", &work.policy_digest, f.now()?)?
        .ok_or("missing claim")?;
    f.store
        .assistance_maintain(claim.deadline - Duration::microseconds(1))?;
    assert!(
        !f.record(&id)?.attempts[0].outcome_unknown,
        "a claim must not expire before its advertised deadline"
    );
    let second = f.enqueue(f.request("second")?)?;
    assert!(
        f.store
            .assistance_claim("ws_fixture", "task_quality", &work.policy_digest, f.now()?)?
            .is_none()
    );
    // A separate connection represents a restarted worker. No sleeps or races.
    let restarted = Store::open(&root.join("orbit.db"))?;
    f.advance(16)?;
    restarted.assistance_maintain(f.now()?)?;
    let after = restarted
        .assistance_record(&id)?
        .ok_or("restart lost work")?;
    assert!(after.attempts[0].outcome_unknown);
    assert_eq!(
        restarted
            .assistance_accounting("synthetic-only/1")?
            .outcome_unknown_attempts,
        1
    );
    let other = restarted
        .assistance_claim("ws_fixture", "task_quality", &work.policy_digest, f.now()?)?
        .ok_or("second claim missing")?;
    assert_eq!(other.work.id, second);
    restarted.assistance_finish(
        &other,
        AssistanceResult::assistance_gap(AssistanceReason::Refused),
        f.now()?,
    )?;
    f.advance(5)?;
    let retry = restarted
        .assistance_claim("ws_fixture", "task_quality", &work.policy_digest, f.now()?)?
        .ok_or("retry missing")?;
    assert_eq!(retry.work.id, id);
    assert_eq!(retry.ordinal, 2);
    assert_ne!(retry.attempt_id, claim.attempt_id);
    assert!(!restarted.assistance_finish(
        &claim,
        AssistanceResult::assistance_gap(AssistanceReason::Refused),
        f.now()?
    )?);
    assert!(restarted.assistance_finish(
        &retry,
        AssistanceResult::assistance_gap(AssistanceReason::Refused),
        f.now()?
    )?);
    let record = restarted
        .assistance_record(&id)?
        .ok_or("missing retry result")?;
    assert_eq!(record.attempts.len(), 2);
    assert_eq!(
        record.result.ok_or("missing current result")?.disposition,
        AssistanceDisposition::Refused
    );
    assert_eq!(
        record.attempts[0]
            .late_result
            .as_ref()
            .ok_or("late result not retained")?
            .disposition,
        AssistanceDisposition::Stale
    );
    assert_eq!(
        restarted
            .assistance_accounting("synthetic-only/1")?
            .reserved_tokens,
        u64::from(work.limits.max_input_tokens) * 3
    );
    f.advance(1_000_000)?;
    restarted.assistance_maintain(f.now()?)?;
    assert!(restarted.assistance_record(&id)?.is_none());
    assert_eq!(
        restarted
            .assistance_accounting("synthetic-only/1")?
            .attempts,
        3
    );
    Ok(())
}

/// Gives `value` all of the first answer's probability mass, so the distribution
/// agrees with a choice of `value`.
fn concentrate_choice_mass(body: &mut Value, value: &str) -> FixtureResult {
    for p in body["answers"][0]["probabilities"]
        .as_array_mut()
        .ok_or("probabilities missing")?
    {
        p["probability"] = json!(if p["value"] == value { 1.0 } else { 0.0 });
    }
    Ok(())
}

fn fixture_validation(root: &Path, schema: &Value) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    for (subject, mut body, reason) in [
        (
            "missing",
            f.good_response(),
            AssistanceReason::MissingAnswers,
        ),
        ("extra", f.good_response(), AssistanceReason::Malformed),
        ("wrong-type", f.good_response(), AssistanceReason::Malformed),
        ("nonfinite", f.good_response(), AssistanceReason::Malformed),
        ("refusal", f.good_response(), AssistanceReason::Refused),
    ] {
        match subject {
            "missing" => {
                body["answers"]
                    .as_array_mut()
                    .ok_or("answers missing")?
                    .pop();
            }
            "extra" => body["answers"]
                .as_array_mut()
                .ok_or("answers missing")?
                .push(json!({"type":"predicate","name":"invented","probability":0.98})),
            "wrong-type" => {
                body["answers"][0] = json!({"type":"choice","name":body["answers"][0]["name"],"choice":"bad","confidence":0.9,"probabilities":[]})
            }
            "nonfinite" => body["answers"][0]["probability"] = json!(2.0),
            "refusal" => {
                body["answers"][0] = json!({"type":"refusal","name":body["answers"][0]["name"]})
            }
            _ => {}
        }
        let id = f.enqueue(f.request(subject)?)?;
        f.respond(body)?;
        f.process()?;
        assert_eq!(
            f.record(&id)?
                .result
                .ok_or("missing validation result")?
                .reason,
            reason
        );
    }
    // JSON's overflowing numeric exponent must be rejected, not rounded.
    let id = f.enqueue(f.request("infinite-json")?)?;
    f.transport.replies.lock().map_err(|_| "replies lock")?.push_back(Ok(AssistanceHttpResponse { status:200,
        body:br#"{"model":"gpt-6-luna","answers":[{"type":"predicate","name":"outcome_unclear","probability":1e999}]}"#.to_vec() }));
    f.process()?;
    assert_eq!(
        f.record(&id)?
            .result
            .ok_or("missing invalid result")?
            .reason,
        AssistanceReason::Malformed
    );
    let mut oversized = f.request("oversized")?;
    oversized
        .evidence_packet
        .fields
        .insert("excerpts".into(), json!("x".repeat(70_000)));
    assert!(f.service.enqueue_assistance(oversized, &[]).is_err());
    // Choice distributions, allowed values and abstention from the evaluator.
    let choice =
        AssistanceFixture::open_fixture(&root.join("choice"), schema, "failure_categorization")?;
    let id = choice.enqueue(choice.request("choice")?)?;
    choice.respond(choice.good_response())?;
    choice.process()?;
    assert_eq!(
        choice
            .record(&id)?
            .result
            .ok_or("missing choice result")?
            .disposition,
        AssistanceDisposition::Suggested
    );
    let mut bad = choice.good_response();
    bad["answers"][0]["choice"] = json!("not-allowed");
    let id = choice.enqueue(choice.request("bad-choice")?)?;
    choice.respond(bad)?;
    choice.process()?;
    assert_eq!(
        choice
            .record(&id)?
            .result
            .ok_or("missing invalid choice")?
            .reason,
        AssistanceReason::Malformed
    );
    // The chosen value must be the most probable one, and its confidence must be
    // that value's probability.
    let mut not_most_probable = choice.good_response();
    not_most_probable["answers"][0]["confidence"] = json!(0.98);
    concentrate_choice_mass(&mut not_most_probable, "misclassified_hold")?;
    let id = choice.enqueue(choice.request("not-most-probable")?)?;
    choice.respond(not_most_probable)?;
    choice.process()?;
    let result = choice
        .record(&id)?
        .result
        .ok_or("missing contradictory choice")?;
    assert_eq!(
        (result.reason, result.disposition),
        (AssistanceReason::Malformed, AssistanceDisposition::Invalid)
    );
    let mut miscalibrated = choice.good_response();
    miscalibrated["answers"][0]["confidence"] = json!(0.5);
    let id = choice.enqueue(choice.request("miscalibrated-confidence")?)?;
    choice.respond(miscalibrated)?;
    choice.process()?;
    let result = choice
        .record(&id)?
        .result
        .ok_or("missing miscalibrated choice")?;
    assert_eq!(
        (result.reason, result.disposition),
        (AssistanceReason::Malformed, AssistanceDisposition::Invalid)
    );
    let mut spread = choice.good_response();
    spread["answers"][0]["confidence"] = json!(0.95);
    spread["answers"][0]["probabilities"][0]["probability"] = json!(0.95);
    spread["answers"][0]["probabilities"][1]["probability"] = json!(0.05);
    let id = choice.enqueue(choice.request("consistent-spread")?)?;
    choice.respond(spread)?;
    choice.process()?;
    assert_eq!(
        choice
            .record(&id)?
            .result
            .ok_or("missing consistent choice")?
            .disposition,
        AssistanceDisposition::Suggested
    );
    let mut abstain = choice.good_response();
    abstain["answers"][0]["choice"] = json!("insufficient_evidence");
    concentrate_choice_mass(&mut abstain, "insufficient_evidence")?;
    let id = choice.enqueue(choice.request("abstain")?)?;
    choice.respond(abstain)?;
    choice.process()?;
    assert_eq!(
        choice
            .record(&id)?
            .result
            .ok_or("missing abstention")?
            .disposition,
        AssistanceDisposition::Uncertain
    );
    Ok(())
}

fn fixture_budgets(root: &Path, schema: &Value) -> FixtureResult {
    for (case, expected) in [
        ("unknown-price", AssistanceReason::PricingUnknown),
        ("tokens", AssistanceReason::BudgetExhausted),
        ("spend", AssistanceReason::BudgetExhausted),
    ] {
        let f = AssistanceFixture::open_fixture(&root.join(case), schema, "task_quality")?;
        {
            let mut s = f.settings.lock().map_err(|_| "settings lock")?;
            match case {
                "unknown-price" => s.budget.input_microusd_per_million_tokens = None,
                "tokens" => s.budget.token_ceiling = 0,
                _ => s.budget.spend_ceiling_microusd = 0,
            }
        }
        let id = f.enqueue(f.request(case)?)?;
        f.process()?;
        assert_eq!(
            f.record(&id)?.result.ok_or("missing budget gap")?.reason,
            expected
        );
        assert!(
            f.transport
                .bodies
                .lock()
                .map_err(|_| "body lock")?
                .is_empty()
        );
    }
    let f = AssistanceFixture::open_fixture(&root.join("queue"), schema, "task_quality")?;
    f.settings
        .lock()
        .map_err(|_| "settings lock")?
        .limits
        .queue_capacity = 1;
    let pending_before_displaced_snapshot = pending_assistance_work(&f)?;
    let id = f.enqueue(f.request("expire")?)?;
    let mut overflow_request = f.request("expire")?;
    overflow_request.source_digest = assistance::assistance_digest(b"overflow");
    assert_eq!(
        f.service
            .enqueue_assistance(overflow_request, &[SUPPLIED.into()])?,
        AssistanceAdmission::QueueFull
    );
    assert_eq!(
        pending_assistance_work(&f)?,
        pending_before_displaced_snapshot,
        "QueueFull left displaced work consuming pending capacity"
    );
    let mut after_overflow = f.request("expire")?;
    after_overflow.source_digest = assistance::assistance_digest(b"after-overflow");
    let later_admission = f
        .service
        .enqueue_assistance(after_overflow, &[SUPPLIED.into()])?;
    let AssistanceAdmission::Enqueued(later_id) = later_admission else {
        return Err(format!("admission after QueueFull was {later_admission:?}").into());
    };
    assert_eq!(
        f.store
            .assistance_accounting("synthetic-only/1")?
            .queue_rejections,
        1
    );
    f.advance(301)?;
    f.store.assistance_maintain(f.now()?)?;
    assert_eq!(
        f.record(&id)?
            .result
            .ok_or("missing superseded gap")?
            .reason,
        AssistanceReason::Superseded
    );
    assert_eq!(
        f.record(&later_id)?
            .result
            .ok_or("missing expired gap")?
            .reason,
        AssistanceReason::QueueExpired
    );
    Ok(())
}

fn pending_assistance_work(f: &AssistanceFixture) -> Result<u32, OrbitError> {
    f.store.with_read_connection(|conn| {
        conn.query_row(
            "SELECT COALESCE(SUM(state!='done'),0) FROM assistance_work",
            [],
            |row| row.get(0),
        )
        .map_err(|_| OrbitError::Store("fixture pending count".into()))
    })
}

fn fixture_config_and_compatibility(root: &Path) -> FixtureResult {
    std::fs::create_dir_all(root)?;
    let global = root.join("global");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&global)?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("config.toml"), "[assistance]\npolicies=[]\n")?;
    assert!(
        orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(&global, &workspace))
            .is_err()
    );
    let mut config = orbit_config::ConfigStore::open(
        orbit_config::ConfigScope::Workspace,
        workspace.join("config.toml"),
    )?;
    assert!(config.set_value("assistance", "{}").is_err());
    let path = root.join("compat.db");
    let store = Store::open(&path)?;
    // Reconstruct exactly the v71 prefix: v72 and v73 add independent tables.
    // Keep the old connection open across the real Store::open migration.
    {
        let connection = store.connection();
        let conn = connection.lock().map_err(|_| "connection lock")?;
        let compat: String = conn.query_row(
            "SELECT value FROM schema_meta WHERE key='migration.compat'",
            [],
            |r| r.get(0),
        )?;
        let mut compat: Value = serde_json::from_str(&compat)?;
        compat["version"] = json!(71);
        conn.execute_batch("DROP TABLE worker_resource_limits; DROP TABLE assistance_slots; DROP TABLE assistance_attempts; DROP TABLE assistance_work; DROP TABLE assistance_budgets;
            DELETE FROM schema_meta WHERE key='migration.v0073'; DELETE FROM schema_meta WHERE key='migration.v0072'; CREATE TABLE old_writer_fixture(value TEXT);")?;
        conn.execute(
            "UPDATE schema_meta SET value=?1 WHERE key='migration.compat'",
            [compat.to_string()],
        )?;
    }
    assert_eq!(store.schema_version()?, 71);
    let upgraded = Store::open(&path)?;
    assert_eq!(upgraded.schema_version()?, 73);
    let connection = store.connection();
    let conn = connection.lock().map_err(|_| "connection lock")?;
    let database = orbit_store::maintenance::migration::LedgeredDatabase {
        component: orbit_store::contracts::StateComponent::StoreSchema,
        noun: "store database",
        foreign_keys_off: false,
        stamp: None,
    };
    let compatible =
        orbit_store::maintenance::migration::evaluate_newer_database(&conn, &database, 73, 71)?;
    assert!(compatible.writable);
    conn.execute_batch("INSERT INTO old_writer_fixture VALUES ('compatible');")?;
    assert_eq!(
        conn.query_row("SELECT value FROM old_writer_fixture", [], |r| r
            .get::<_, String>(0))?,
        "compatible"
    );
    Ok(())
}

fn fixture_modes_and_late_results(root: &Path, schema: &Value) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    f.settings.lock().map_err(|_| "settings lock")?.policies[0].mode = AssistanceMode::Shadow;
    let request = f.request("shadow")?;
    let source = request.source_digest.clone();
    let id = f.enqueue(request)?;
    f.respond(f.good_response())?;
    f.process()?;
    assert!(f.record(&id)?.result.is_some());
    assert!(f.service.assistance_advisory(&id, &source)?.is_none());
    f.settings.lock().map_err(|_| "settings lock")?.policies[0].mode = AssistanceMode::Advisory;
    assert!(
        !f.service
            .assistance_assessment(&id, &source)?
            .ok_or("missing shadow history")?
            .current
    );
    let late = f.enqueue(f.request("late-timeout")?)?;
    let clock = f.clock.clone();
    *f.transport.on_send.lock().map_err(|_| "hook lock")? = Some(Box::new(move || {
        if let Ok(mut now) = clock.lock() {
            *now += Duration::seconds(16);
        }
    }));
    f.respond(f.good_response())?;
    f.process()?;
    let record = f.record(&late)?;
    assert_eq!(
        record.result.ok_or("missing timeout")?.reason,
        AssistanceReason::TimeoutUnknown
    );
    assert!(record.attempts[0].outcome_unknown);
    f.advance(5)?;
    f.respond(f.good_response())?;
    f.process()?;
    assert_eq!(f.record(&late)?.attempts.len(), 2);
    assert_eq!(
        f.record(&late)?.result.ok_or("missing retry")?.disposition,
        AssistanceDisposition::Suggested
    );
    // A newer semantic snapshot fences an older in-flight result atomically.
    let request = f.request("edited-subject")?;
    let old = f.enqueue(request.clone())?;
    let work = f.record(&old)?.work;
    let claim = f
        .store
        .assistance_claim("ws_fixture", "task_quality", &work.policy_digest, f.now()?)?
        .ok_or("edit claim missing")?;
    let mut newer = request;
    newer.source_digest = assistance::assistance_digest(b"new semantic revision");
    let new = f.enqueue(newer)?;
    assert_ne!(old, new);
    assert!(!f.store.assistance_finish(
        &claim,
        AssistanceResult::assistance_gap(AssistanceReason::Refused),
        f.now()?
    )?);
    assert!(!f.record(&old)?.current);
    f.respond(f.good_response())?;
    f.process()?;
    assert_eq!(
        f.record(&new)?.result.ok_or("missing latest")?.disposition,
        AssistanceDisposition::Suggested
    );
    // The dedicated credential namespace is excluded even by explicit passes.
    assert!(
        orbit_common::security::child_env::allowlisted_child_env_from(
            &[("ORBIT_ASSISTANCE_FIXTURE_KEY".into(), CREDENTIAL.into())],
            &["ORBIT_ASSISTANCE_FIXTURE_KEY".into()],
            &["ORBIT_ASSISTANCE_FIXTURE_KEY"]
        )
        .iter()
        .all(|(name, value)| name != "ORBIT_ASSISTANCE_FIXTURE_KEY" && value != CREDENTIAL)
    );
    Ok(())
}

fn fixture_adapter_identity(root: &Path, schema: &Value) -> FixtureResult {
    // The recorded adapter identity must come from the injected adapter, not
    // from a service-owned literal, so a second adapter is never recorded
    // under the OpenAI identity.
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    let id = f.enqueue(f.request("adapter-identity")?)?;
    let record = f.record(&id)?;
    assert_eq!(
        record.work.adapter,
        f.adapter.assistance_adapter_identity(),
        "recorded adapter identity must equal the injected adapter's identity"
    );
    assert_eq!(
        record.work.adapter, OPENAI_DECISIONS_ADAPTER_IDENTITY,
        "OpenAI Decisions adapter identity must stay stable"
    );
    // A second adapter must be recorded under its own identity, never the
    // OpenAI identity.
    let second = Arc::new(SecondAssistanceAdapter::with_identity(
        f.adapter.clone(),
        "synthetic-assistance/1",
    ));
    let service = f.service_with_assistance_adapter(second.clone());
    let second_id =
        match service.enqueue_assistance(f.request("second-adapter")?, &[SUPPLIED.into()])? {
            AssistanceAdmission::Enqueued(id) | AssistanceAdmission::Existing(id) => id,
            _ => return Err("second adapter fixture was not admitted".into()),
        };
    let second_record = f.record(&second_id)?;
    assert_eq!(
        second_record.work.adapter,
        second.assistance_adapter_identity(),
        "second adapter must be recorded under its own identity"
    );
    assert_ne!(
        second_record.work.adapter, OPENAI_DECISIONS_ADAPTER_IDENTITY,
        "second adapter must not be recorded under the OpenAI identity"
    );
    Ok(())
}

fn main() -> FixtureResult {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "--fixture-child") {
        let schema: Value =
            serde_json::from_slice(&std::fs::read(args.get(2).ok_or("schema path required")?)?)?;
        let root = Path::new(args.get(3).ok_or("isolated root required")?);
        fixture_service_boundaries(&root.join("service"), &schema)?;
        fixture_adapter_identity(&root.join("adapter-identity"), &schema)?;
        fixture_completed_result_outlives_queue_age(&root.join("completed"), &schema)?;
        fixture_expired_queued_work_reads_match_maintenance(&root.join("expired"), &schema)?;
        fixture_send_failure_reconciliation(&root.join("send-failure"), &schema)?;
        fixture_http_status_retry(&root.join("http-status"), &schema)?;
        fixture_credential_refusal_releases_reservation(&root.join("credential"), &schema)?;
        fixture_settings_read_failure_releases_reservation(&root.join("settings-read"), &schema)?;
        fixture_deadline_before_send_is_not_unknown(&root.join("deadline"), &schema)?;
        fixture_restart_retry(&root.join("restart"), &schema)?;
        fixture_validation(&root.join("validation"), &schema)?;
        fixture_budgets(&root.join("bounds"), &schema)?;
        fixture_modes_and_late_results(&root.join("modes"), &schema)?;
        fixture_config_and_compatibility(&root.join("config"))?;
        std::io::stdout().write_all(b"{\"fixture\":\"decision-assistance/1\",\"status\":\"passed\",\"network_calls\":0,\"labels\":\"synthetic\"}\n")?;
        return Ok(());
    }
    let schema = std::fs::canonicalize(args.get(1).ok_or("pass the evaluator schema JSON path")?)?;
    let scratch = schema.parent().ok_or("schema parent required")?;
    let root = tempfile::Builder::new()
        .prefix("assistance-fixture-")
        .tempdir_in(scratch)?;
    let output = Command::new(std::env::current_exe()?)
        .arg("--fixture-child")
        .arg(schema)
        .arg(root.path())
        .env_clear()
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path())
        .output()?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        return Err("isolated assistance fixture failed".into());
    }
    Ok(())
}
