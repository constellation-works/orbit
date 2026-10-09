//! [ORB-14695] A local run whose provider account hit a usage limit holds its
//! task in the backlog, away from every crew of that provider, until the
//! reset the provider reported, and records the limit in the host's store.
//!
//! The end-to-end fixture needs a Unix shell for its fake Antigravity CLI;
//! the hold tests fail a run with a recorded step failure and compile on
//! every platform.

use chrono::{DateTime, Duration, Utc};
use orbit_core::TaskStatus;
use orbit_types::workflow::{ProviderFailureClass, ProviderLimitFailure};

use super::dispatch_admission::isolated;
use super::provider_failure_hold::{Fixture, fixture};

/// Two Antigravity crews in the task's complexity pool and a Claude default.
const ANTIGRAVITY_CREWS: &str = r#"[workflow]
default_crew = "opus"
medium_complexity_crews = ["gemini-flash", "gemini-pro"]

[crews.gemini-flash]
provider = "antigravity"
model = "gemini-3.8-flash-high"

[crews.gemini-pro]
provider = "antigravity"
model = "gemini-3.8-pro-high"

[crews.opus]
provider = "claude"
model = "opus-model"
"#;

/// Two Codex crews on different models and a Claude default. The fixture's
/// tasks name their crew, so the `pool` policy lets a limit hold redirect
/// them [ORB-14697].
const CODEX_MODELS: &str = r#"[workflow]
default_crew = "opus"
medium_complexity_crews = ["sol", "luna"]
provider_limit_explicit_crews = "pool"

[crews.sol]
provider = "codex"
model = "gpt-5.1-codex-max"

[crews.luna]
provider = "codex"
model = "gpt-5.4"

[crews.opus]
provider = "claude"
model = "opus-model"
"#;

/// What Antigravity wrote for crew `gemini-flash` (runs
/// jrun-20260920-0742-c19 and -c21).
const ANTIGRAVITY_QUOTA: &str = "Individual quota reached. Please upgrade your subscription to \
     increase your limits. Resets in 1h37m37s.";

/// A step failure carrying `limit` on `provider`.
fn limit_failure(provider: &str, limit: &ProviderLimitFailure) -> String {
    format!(
        "step `implement_one`: {}",
        limit.text(
            provider,
            &format!("{provider} provider reported a usage limit: {ANTIGRAVITY_QUOTA}")
        )
    )
}

fn fail_with_limit(fx: &Fixture, task: &str, crew: &str, limit: &ProviderLimitFailure) {
    let run = fx.admit(task, crew);
    fx.fail(&run, &limit_failure(crew_provider(crew), limit));
}

fn crew_provider(crew: &str) -> &'static str {
    match crew {
        "sol" | "luna" => "codex",
        _ => "antigravity",
    }
}

fn resetting_at(resets_at: DateTime<Utc>) -> ProviderLimitFailure {
    ProviderLimitFailure {
        resets_at: Some(resets_at),
        ..ProviderLimitFailure::default()
    }
}

/// The incident end to end: a fake Antigravity prints its quota error, the
/// engine types it and skips both recoveries, and Core holds the task until
/// the reported reset with every Antigravity crew excluded. The host's store
/// holds the limit. The task names its crew, so under the default `wait`
/// policy it waits out the hold rather than move to the Claude crew
/// [ORB-14697].
#[cfg(unix)]
#[test]
fn an_antigravity_quota_holds_every_antigravity_crew_until_the_reported_reset() {
    use std::sync::Arc;

    use orbit_engine::{V2AuditWriter, execute_job_with_resume};
    use orbit_types::telemetry::ProviderLimitSource;
    use orbit_types::workflow::activity_job::Provider;
    use orbit_types::workflow::is_provider_limit;
    use serde_json::json;

    use super::provider_failure_hold::{FakeCodex, Pipeline, implementation_job};

    if !isolated(
        "provider_limit_hold::an_antigravity_quota_holds_every_antigravity_crew_until_the_reported_reset",
    ) {
        return;
    }
    let fx = fixture(ANTIGRAVITY_CREWS);
    let task = fx.task("gemini-flash");
    let run = fx.admit(&task, "gemini-flash");
    let stdout = format!(
        r#"{{"status":"ERROR","response":"","error":{}}}"#,
        serde_json::to_string(ANTIGRAVITY_QUOTA).unwrap()
    );
    let agy = FakeCodex::named(&fx.repo, "agy", &stdout);
    let host = Pipeline::new(&fx, agy.path.clone());
    let audit = V2AuditWriter::with_disk_sinks(
        &fx.repo.join("audit"),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_fixture",
        &run,
        "fixture",
        Some(&fx.repo),
    )
    .unwrap();
    let before = Utc::now();
    let outcome = execute_job_with_resume(
        &implementation_job(Provider::Antigravity),
        json!({ "prompt": "implement", "task_ids": [task] }),
        &run,
        audit,
        &host,
        None,
    );
    let after = Utc::now();
    let message = match &outcome {
        Ok(outcome) => {
            assert!(!outcome.success, "{outcome:?}");
            outcome.message.clone().unwrap_or_default()
        }
        Err(error) => error.to_string(),
    };
    assert!(is_provider_limit(None, Some(&message)), "{message}");
    let actions = host.actions.lock().unwrap().clone();
    assert!(
        !actions.iter().any(|action| action == "step_fix"),
        "step recovery is skipped: {actions:?}"
    );
    assert!(
        !actions.iter().any(|action| action == "decide"),
        "final recovery is skipped: {actions:?}"
    );
    assert_eq!(*host.final_recovery_admissions.lock().unwrap(), 0);
    fx.fail(&run, &message);

    let reset = Duration::hours(1) + Duration::minutes(37) + Duration::seconds(37);
    let resets_at = ProviderLimitFailure::from_text(&message)
        .and_then(|limit| limit.resets_at)
        .expect("the failure carries the reported reset");
    assert!(
        resets_at >= before + reset - Duration::seconds(1) && resets_at <= after + reset,
        "{resets_at}"
    );

    assert_eq!(fx.status(&task), TaskStatus::Backlog);
    let hold = fx.last_hold(&task);
    assert_eq!(hold.class, ProviderFailureClass::Limit);
    assert_eq!(hold.provider.as_deref(), Some("antigravity"));
    assert_eq!(
        hold.excluded_crews,
        ["gemini-flash", "gemini-pro"],
        "every Antigravity crew"
    );
    assert_eq!(hold.not_before, resets_at, "held until the reported reset");
    assert_eq!(hold.run_id, run);

    let limits = fx.runtime.provider_limits().unwrap();
    let [observation] = limits.as_slice() else {
        panic!("one observation for the provider: {limits:?}");
    };
    assert_eq!(observation.provider, "antigravity");
    assert!(observation.exhausted);
    assert_eq!(observation.source, ProviderLimitSource::Error);
    assert_eq!(observation.resets_at, Some(resets_at));
    assert_eq!(observation.run_id.as_deref(), Some(run.as_str()));

    assert!(!fx.admitted(&task, &[]), "the explicit crew waits");
    let deferred = fx.exclusion(&task, &[]);
    assert_eq!(deferred["reason"], "provider_backoff", "{deferred}");
    assert!(
        deferred["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(&resets_at.to_rfc3339())),
        "the deferral names the reset: {deferred}"
    );
}

/// A limit that reported no reset backs off from 30 minutes, doubling with
/// each limit hold on that provider, up to 6 hours.
#[test]
fn a_limit_without_a_reported_reset_backs_off_from_thirty_minutes_to_six_hours() {
    if !isolated(
        "provider_limit_hold::a_limit_without_a_reported_reset_backs_off_from_thirty_minutes_to_six_hours",
    ) {
        return;
    }
    let fx = fixture(ANTIGRAVITY_CREWS);
    let task = fx.task("gemini-flash");
    for minutes in [30, 60, 120, 240, 360, 360] {
        let before = Utc::now();
        fail_with_limit(&fx, &task, "gemini-flash", &ProviderLimitFailure::default());
        let after = Utc::now();
        let hold = fx.last_hold(&task);
        assert_eq!(hold.class, ProviderFailureClass::Limit);
        assert_eq!(hold.excluded_crews, ["gemini-flash", "gemini-pro"]);
        let backoff = Duration::minutes(minutes);
        assert!(
            hold.not_before >= before + backoff && hold.not_before <= after + backoff,
            "a {minutes}-minute backoff: {hold:?}"
        );
    }
}

/// A reported reset is clamped to between 5 minutes and 7 days from the
/// failure.
#[test]
fn a_reported_reset_is_clamped_to_five_minutes_through_seven_days() {
    if !isolated(
        "provider_limit_hold::a_reported_reset_is_clamped_to_five_minutes_through_seven_days",
    ) {
        return;
    }
    for (reported, bound) in [
        (Duration::minutes(-10), Duration::minutes(5)),
        (Duration::days(30), Duration::days(7)),
        (Duration::hours(3), Duration::hours(3)),
    ] {
        let fx = fixture(ANTIGRAVITY_CREWS);
        let task = fx.task("gemini-flash");
        let before = Utc::now();
        fail_with_limit(&fx, &task, "gemini-flash", &resetting_at(before + reported));
        let after = Utc::now();
        let hold = fx.last_hold(&task);
        // The failure text carries whole seconds.
        assert!(
            hold.not_before >= before + bound - Duration::seconds(1)
                && hold.not_before <= after + bound,
            "reported {reported}, held {bound}: {hold:?}"
        );
    }
}

/// A limit that names a model excludes only the crews on that model; one
/// naming a model no crew runs excludes every crew of the provider.
#[test]
fn a_model_scoped_limit_excludes_only_the_crews_on_that_model() {
    if !isolated("provider_limit_hold::a_model_scoped_limit_excludes_only_the_crews_on_that_model")
    {
        return;
    }
    for (model, excluded, drawn) in [
        ("gpt-5.1-codex-max", vec!["sol"], "luna"),
        ("gpt-9", vec!["luna", "sol"], "opus"),
    ] {
        let fx = fixture(CODEX_MODELS);
        let task = fx.task("sol");
        let limit = ProviderLimitFailure {
            model: Some(model.to_string()),
            ..resetting_at(Utc::now() + Duration::hours(2))
        };
        fail_with_limit(&fx, &task, "sol", &limit);
        let hold = fx.last_hold(&task);
        assert_eq!(hold.excluded_crews, excluded, "{model}: {hold:?}");
        assert_eq!(fx.exclusion(&task, &["sol"])["crew"], drawn, "{model}");
    }
}
