//! [ORB-17288] A crew that sets `env` runs on its own provider account, so
//! its usage-limit holds, readings and budget are its own, and its
//! provider's default login's are not its.
//!
//! The crews are three Claude crews, one of which, `opus-credits`, sets
//! `env` in the global file. Failures are recorded the way a run's worker
//! records them, readings and spend the way a run's telemetry does, and
//! admission is read through `list_backlog_tasks`, the action every local
//! drain runs.

use chrono::{Duration, Utc};
use orbit_core::TaskStatus;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::telemetry::{InvocationTrace, ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{
    PROVIDER_FAILURE_HOLD_EVENT, PROVIDER_FAILURE_HOLD_MARKER, ProviderFailureClass,
    ProviderLimitFailure, provider_failure_text, with_failed_account,
};

use super::provider_failure_hold::{Fixture, fixture_with_global};

/// Two crews on the Claude subscription and one on its own API key.
const GLOBAL: &str = r#"[crews.opus]
provider = "claude"
model = "claude-opus-5-5"

[crews.sonnet]
provider = "claude"
model = "claude-sonnet-5-5"

[crews.opus-credits]
provider = "claude"
model = "claude-opus-5-5"
env = ["ANTHROPIC_API_KEY=CREW_ACCOUNT_FIXTURE_KEY"]
"#;

/// The task's pool holds a crew of each account; `pool` lets a limit hold
/// redraw a task whose crew is its own choice [ORB-14697].
fn workspace(extra: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"opus\"\n\
         medium_complexity_crews = [\"opus\", \"opus-credits\"]\n\
         provider_limit_explicit_crews = \"pool\"\n{extra}\n"
    )
}

/// A Claude usage-limit step failure resetting in two hours, as the crew
/// `account` reports it when that is a crew account.
fn usage_limit(account: Option<&str>) -> String {
    let limit = ProviderLimitFailure {
        window: Some("five_hour".into()),
        resets_at: Some(Utc::now() + Duration::hours(2)),
        ..ProviderLimitFailure::default()
    };
    let text = limit.text("claude", "claude provider reported a usage limit");
    let text = match account {
        Some(account) => with_failed_account(&text, account),
        None => text,
    };
    format!("step `implement_one`: {text}")
}

/// A live exhausted Claude `five_hour` reading of `account`, recorded by a
/// run of `crew`.
fn exhausted(account: Option<&str>, crew: &str) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: "claude".into(),
        account: account.map(ToOwned::to_owned),
        model: None,
        window: Some("five_hour".into()),
        exhausted: true,
        source: ProviderLimitSource::Event,
        resets_at: Some(Utc::now() + Duration::hours(2)),
        used_percent: Some(100.0),
        window_minutes: Some(300),
        gating: true,
        partial: false,
        observed_at: Utc::now(),
        run_id: None,
        crew: Some(crew.into()),
        detail: String::new(),
    }
}

/// Record a Claude invocation costing `cost` dollars under `account`.
fn spend(fx: &Fixture, account: Option<&str>, cost: f64) {
    fx.runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "fixture-run".into(),
            account: account.map(ToOwned::to_owned),
            activity_id: "implement_one".into(),
            agent: "claude".into(),
            provider: Some("claude".into()),
            model: None,
            task_ids: Vec::new(),
            trace: InvocationTrace {
                provider_cost_usd: Some(cost),
                ..InvocationTrace::default()
            },
        })
        .unwrap();
}

/// A limit on the subscription holds only the crews without `env`, and the
/// credits crew takes the task in the same pass; a limit the credits crew
/// reports holds only it. An authentication failure of the credits crew
/// keeps this host off that crew alone.
fn a_usage_limit_holds_only_the_crews_of_the_failing_account() {
    let fx = fixture_with_global(GLOBAL, &workspace(""));

    let subscription = fx.task("opus");
    let run = fx.admit(&subscription, "opus");
    fx.fail(&run, &usage_limit(None));
    let hold = fx.last_hold(&subscription);
    assert_eq!(hold.class, ProviderFailureClass::Limit);
    assert_eq!(hold.account, None);
    assert_eq!(
        hold.excluded_crews,
        ["opus", "sonnet"],
        "every crew on the subscription, not the credits crew"
    );

    let credits = fx.task("opus-credits");
    let run = fx.admit(&credits, "opus-credits");
    fx.fail(&run, &usage_limit(Some("opus-credits")));
    let hold = fx.last_hold(&credits);
    assert_eq!(hold.account.as_deref(), Some("opus-credits"));
    assert_eq!(
        hold.excluded_crews,
        ["opus-credits"],
        "the credits crew alone"
    );

    assert!(
        fx.admitted(&subscription, &["opus-credits"]) && !fx.admitted(&subscription, &["opus"]),
        "the subscription's limit leaves the credits crew dispatching; allowing the \
         subscription's crew does not allow the credits crew on the same model"
    );
    assert!(
        fx.admitted(&credits, &["opus"]) && !fx.admitted(&credits, &["opus-credits"]),
        "the credits crew's limit leaves the subscription dispatching"
    );

    // An expired key on the credits crew is that crew's outage on this host,
    // not the subscription's.
    let expired = fx.task("opus-credits");
    let run = fx.admit(&expired, "opus-credits");
    let unavailable = with_failed_account(
        &provider_failure_text(
            ProviderFailureClass::Unavailable,
            "claude",
            "claude provider authentication failure (HTTP 401)",
        ),
        "opus-credits",
    );
    fx.fail(&run, &format!("step `implement_one`: {unavailable}"));
    assert_eq!(fx.last_hold(&expired).excluded_crews, ["opus-credits"]);
    let other = fx.task("opus");
    assert!(
        fx.admitted(&other, &["opus"]),
        "the credits crew's outage leaves the subscription crews drawable"
    );
}

/// A hold stored before accounts were named carries no account and keeps
/// excluding exactly the crews it lists, the credits crew included.
fn a_hold_stored_before_accounts_were_named_applies_as_written() {
    let fx = fixture_with_global(GLOBAL, &workspace(""));
    let task = fx.task("opus");
    let not_before = (Utc::now() + Duration::hours(2)).to_rfc3339();
    let legacy = format!(
        "{PROVIDER_FAILURE_HOLD_MARKER} {{\"class\":\"limit\",\"provider\":\"claude\",\
         \"excluded_crews\":[\"opus\",\"opus-credits\",\"sonnet\"],\
         \"not_before\":\"{not_before}\",\"run_id\":\"jrun-legacy\"}} legacy fixture hold"
    );
    for (status, event, note) in [
        (TaskStatus::InProgress, None, None),
        (
            TaskStatus::Backlog,
            Some(PROVIDER_FAILURE_HOLD_EVENT.to_string()),
            Some(legacy),
        ),
    ] {
        fx.runtime
            .apply_task_automation_update(
                &task,
                TaskAutomationUpdate {
                    status: Some(status),
                    status_event: event,
                    status_note: note,
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let hold = fx.last_hold(&task);
    assert_eq!(hold.account, None);
    assert!(
        !fx.admitted(&task, &["opus-credits"]),
        "the stored hold still excludes every crew it names"
    );
    assert_eq!(
        fx.exclusion(&task, &[])["reason"],
        "provider_backoff",
        "every crew the task may run as is held"
    );
}

/// A reading of the subscription's window limits its crews and not the
/// credits crew, even when the credits crew's run recorded it before
/// accounts were named; a reading of the credits account limits it alone.
fn a_default_login_reading_does_not_limit_a_crew_account() {
    let fx = fixture_with_global(GLOBAL, &workspace(""));
    let task = fx.tagged_task(&[], None).id;
    fx.runtime
        .record_provider_limit(&exhausted(None, "opus-credits"))
        .unwrap();
    let view = fx.runtime.provider_limits_view(Utc::now());
    for crew in ["opus", "sonnet"] {
        assert!(
            view.crew(crew).is_some_and(|limit| limit.gated),
            "{crew}: {view:?}"
        );
    }
    assert!(view.crew("opus-credits").is_none(), "{view:?}");
    assert_eq!(
        fx.exclusion(&task, &["sonnet"])["crew"],
        "opus-credits",
        "the pool draw skips the limited subscription crew"
    );

    fx.runtime
        .record_provider_limit(&exhausted(Some("opus-credits"), "opus-credits"))
        .unwrap();
    let view = fx.runtime.provider_limits_view(Utc::now());
    let credits = view
        .crew("opus-credits")
        .expect("the account's own reading");
    assert!(credits.gated);
    assert_eq!(credits.account.as_deref(), Some("opus-credits"));
    let status = view
        .providers
        .iter()
        .find(|status| status.account.as_deref() == Some("opus-credits"))
        .expect("the crew account has its own status");
    assert_eq!(
        (status.label(), status.crews.as_slice(), status.gated),
        (
            "claude/opus-credits".to_string(),
            ["opus-credits".to_string()].as_slice(),
            true
        )
    );
    assert_eq!(fx.exclusion(&task, &[])["reason"], "provider_limit");
}

/// A crew budget reads only that crew account's spend: the subscription's
/// spend leaves it at zero, its own gates it at the threshold, and it gates
/// nothing on the subscription.
fn a_crew_budget_gates_the_crew_from_its_own_ledger_spend() {
    let fx = fixture_with_global(
        GLOBAL,
        &workspace("provider_limit_budgets = [\"opus-credits:200usd/30d\"]"),
    );
    let task = fx.tagged_task(&[], None).id;
    spend(&fx, None, 5_000.0);
    let credits = |fx: &Fixture| {
        fx.runtime
            .provider_limits_view(Utc::now())
            .crew("opus-credits")
            .cloned()
            .expect("the budget reads the crew account")
    };
    let limit = credits(&fx);
    assert_eq!(
        (limit.used_percent, limit.gated),
        (Some(0.0), false),
        "the subscription's spend is not the account's"
    );

    spend(&fx, Some("opus-credits"), 150.0);
    assert_eq!(credits(&fx).used_percent, Some(75.0));

    spend(&fx, Some("opus-credits"), 40.0);
    let limit = credits(&fx);
    assert!(limit.gated, "95% of the budget is at the 90% threshold");
    let view = fx.runtime.provider_limits_view(Utc::now());
    assert!(
        view.crew("opus").is_none(),
        "the budget gates nothing on the subscription: {view:?}"
    );
    assert_eq!(
        fx.exclusion(&task, &["sonnet"])["crew"],
        "opus",
        "the pool draw skips the budgeted crew"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    crew_account_scenarios,
    a_usage_limit_holds_only_the_crews_of_the_failing_account,
    a_hold_stored_before_accounts_were_named_applies_as_written,
    a_default_login_reading_does_not_limit_a_crew_account,
    a_crew_budget_gates_the_crew_from_its_own_ledger_spend
);
