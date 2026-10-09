//! [ORB-14697] A follower whose provider-limit store reads a provider at its
//! usage limit leaves that provider's crews out of its pull drain's window
//! until the reading lapses, then runs them again in the same drain.

use chrono::Duration;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};

use super::*;

const CREWS: &str = "\
[workflow]
default_crew = \"sol\"

[crews.sol]
provider = \"codex\"
model = \"gpt-sol\"

[crews.luna]
provider = \"codex\"
model = \"gpt-luna\"

[crews.opus]
provider = \"claude\"
model = \"claude-opus\"
";

/// A Codex usage-window reading of `used_percent` resetting at `resets_at`,
/// as the Codex telemetry records one after a run.
fn codex_reading(used_percent: f64, resets_at: chrono::DateTime<Utc>) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: "codex".into(),
        model: None,
        window: Some("primary".into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(resets_at),
        used_percent: Some(used_percent),
        window_minutes: Some(300),
        gating: true,
        observed_at: Utc::now(),
        run_id: None,
        crew: None,
        detail: String::new(),
    }
}

/// The window's exclusions of `crew`.
fn exclusions<'a>(pass: &'a Value, crew: &str) -> Vec<&'a Value> {
    pass["crews"]["excluded"]
        .as_array()
        .expect("the pass reports its crew window")
        .iter()
        .filter(|exclusion| exclusion["crew"] == crew)
        .collect()
}

#[test]
fn a_limited_provider_sits_out_the_window_only_until_its_reading_lapses() {
    if !isolated(
        module_path!(),
        "a_limited_provider_sits_out_the_window_only_until_its_reading_lapses",
    ) {
        return;
    }
    let pair = Pair::with_configs(CREWS, CREWS, &[Some("sol")]);
    let drain = pair.run_drain();
    let resets_at = Utc::now() + Duration::hours(2);
    pair.follower
        .record_provider_limit(&codex_reading(95.0, resets_at))
        .expect("seed the follower's reading");

    let limited = pair.pass(&drain);
    for crew in ["sol", "luna"] {
        let [exclusion] = exclusions(&limited, crew)[..] else {
            panic!("{crew} is excluded once: {limited:#}");
        };
        assert_eq!(exclusion["source"], "provider_limit", "{exclusion}");
        assert_eq!(
            exclusion["until"]
                .as_str()
                .and_then(|until| chrono::DateTime::parse_from_rfc3339(until).ok()),
            Some(resets_at.into()),
            "{exclusion}"
        );
        let reason = exclusion["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("codex primary at 95% (limit 90%)"),
            "{reason}"
        );
    }
    assert!(exclusions(&limited, "opus").is_empty(), "{limited:#}");
    let runnable = limited["crews"]["runnable"].clone();
    assert_eq!(runnable, json!(["opus"]), "{limited:#}");
    assert!(
        pair.owner_claims().is_empty(),
        "the sol task waits: {limited:#}"
    );

    // The window rolled over: the next reading's reset has passed.
    pair.follower
        .record_provider_limit(&codex_reading(95.0, Utc::now() - Duration::seconds(1)))
        .expect("record the lapsed reading");
    let lapsed = pair.pass(&drain);
    for crew in ["sol", "luna"] {
        assert!(exclusions(&lapsed, crew).is_empty(), "{lapsed:#}");
        assert!(
            lapsed["crews"]["runnable"]
                .as_array()
                .is_some_and(|runnable| runnable.iter().any(|name| name == crew)),
            "{crew} runs again in the same drain: {lapsed:#}"
        );
    }
    // This binary cannot launch the leaf, but the owner has bound its claim.
    let claims = pair.owner_claims();
    let [claim] = claims.as_slice() else {
        panic!("one claim: {claims:#?}");
    };
    assert_eq!(
        claim["claim"]["task_id"], pair.tasks[0],
        "the sol task is claimed: {lapsed:#}"
    );
}
