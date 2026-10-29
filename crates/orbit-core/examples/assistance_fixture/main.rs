//! Manual isolated public-boundary fixture. Pass the evaluator schema JSON.
//! No live HTTP or promotional credits; no automated feature tests are added.
mod fixture;
use chrono::Duration;
use fixture::{AssistanceFixture, CREDENTIAL, FixtureResult};
use orbit_core::adapter::decisions::AssistanceHttpResponse;
use orbit_store::Store;
use orbit_store::contracts::assistance::{AssistanceAdmission, AssistanceStoreBackend};
use orbit_types::assistance::{
    self, AssistanceDisposition, AssistanceMode, AssistanceReason, AssistanceResult,
};
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;

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
    let mut abstain = choice.good_response();
    abstain["answers"][0]["choice"] = json!("insufficient_evidence");
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
    let id = f.enqueue(f.request("expire")?)?;
    assert_eq!(
        f.service.enqueue_assistance(f.request("overflow")?, &[])?,
        AssistanceAdmission::QueueFull
    );
    assert_eq!(
        f.store
            .assistance_accounting("synthetic-only/1")?
            .queue_rejections,
        1
    );
    f.advance(301)?;
    f.store.assistance_maintain(f.now()?)?;
    assert_eq!(
        f.record(&id)?.result.ok_or("missing expired gap")?.reason,
        AssistanceReason::QueueExpired
    );
    Ok(())
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
    // Reconstruct exactly the v71 prefix: v72 adds only independent tables.
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
        conn.execute_batch("DROP TABLE assistance_slots; DROP TABLE assistance_attempts; DROP TABLE assistance_work; DROP TABLE assistance_budgets;
            DELETE FROM schema_meta WHERE key='migration.v0072'; CREATE TABLE old_writer_fixture(value TEXT);")?;
        conn.execute(
            "UPDATE schema_meta SET value=?1 WHERE key='migration.compat'",
            [compat.to_string()],
        )?;
    }
    assert_eq!(store.schema_version()?, 71);
    let upgraded = Store::open(&path)?;
    assert_eq!(upgraded.schema_version()?, 72);
    let connection = store.connection();
    let conn = connection.lock().map_err(|_| "connection lock")?;
    let database = orbit_store::maintenance::migration::LedgeredDatabase {
        component: orbit_store::contracts::StateComponent::StoreSchema,
        noun: "store database",
        foreign_keys_off: false,
        stamp: None,
    };
    let compatible =
        orbit_store::maintenance::migration::evaluate_newer_database(&conn, &database, 72, 71)?;
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

fn main() -> FixtureResult {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "--fixture-child") {
        let schema: Value =
            serde_json::from_slice(&std::fs::read(args.get(2).ok_or("schema path required")?)?)?;
        let root = Path::new(args.get(3).ok_or("isolated root required")?);
        fixture_service_boundaries(&root.join("service"), &schema)?;
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
