//! `workflow.provider_limit_*` [ORB-14697]: `orbit config set` and a config
//! load admit and reject the same values, through the same registry rows.
#![allow(clippy::expect_used, missing_docs)]

use orbit_config::{
    ConfigRoots, ConfigScope, ConfigStore, ProviderLimitBudgetUnit, ProviderLimitExplicitCrews,
    ProviderLimitPolicy, ResolvedConfig,
};

const OVERRIDES: &str = "workflow.provider_limit_overrides";
const BUDGETS: &str = "workflow.provider_limit_budgets";

/// The policy one load of `[workflow]` `body` admits, or its error.
fn load(body: &str) -> Result<ProviderLimitPolicy, String> {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    std::fs::write(
        workspace.path().join("config.toml"),
        format!("[workflow]\n{body}\n"),
    )
    .expect("write config");
    ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .map(|config| config.snapshot.provider_limit_policy())
        .map_err(|error| error.to_string())
}

/// `orbit config set --global key value`, validated as the command does.
fn set(key: &str, value: &str) -> Result<(), String> {
    let root = tempfile::tempdir().expect("config tempdir");
    let mut store = ConfigStore::open(ConfigScope::Global, root.path().join("config.toml"))
        .map_err(|error| error.to_string())?;
    store
        .set_value(key, value)
        .and_then(|()| store.validate_for_set(key))
        .map_err(|error| error.to_string())
}

#[test]
fn unset_keys_gate_at_ninety_percent_and_keep_explicit_crews_waiting() {
    let policy = load("").expect("an empty [workflow] loads");
    assert_eq!(policy, ProviderLimitPolicy::default());
    assert_eq!(policy.max_used_pct, 90);
    assert_eq!(policy.explicit_crews, ProviderLimitExplicitCrews::Wait);
    assert_eq!(policy.threshold("claude"), 90);
}

#[test]
fn overrides_are_admitted_under_their_canonical_provider() {
    let policy = load(
        "provider_limit_max_used_pct = 80\n\
         provider_limit_overrides = [\"anthropic:95\", \"grok:100\"]\n\
         provider_limit_explicit_crews = \"pool\"",
    )
    .expect("valid provider limit settings load");
    assert_eq!(policy.max_used_pct, 80);
    assert_eq!(policy.threshold("claude"), 95, "anthropic is claude");
    assert_eq!(policy.threshold("anthropic"), 95);
    assert_eq!(policy.threshold("grok"), 100);
    assert_eq!(policy.threshold("codex"), 80, "the default threshold");
    assert_eq!(policy.explicit_crews, ProviderLimitExplicitCrews::Pool);
    set(OVERRIDES, r#"["claude:80", "grok:100"]"#).expect("config set admits valid overrides");
}

#[test]
fn invalid_provider_limit_settings_are_refused_by_load_and_config_set() {
    for (key, value, expected) in [
        (OVERRIDES, r#"["claude:0"]"#, "1..=100"),
        (OVERRIDES, r#"["claude:101"]"#, "1..=100"),
        (OVERRIDES, r#"["claude:high"]"#, "1..=100"),
        (OVERRIDES, r#"["nobody:80"]"#, "unknown provider 'nobody'"),
        (OVERRIDES, r#"["claude:80", "grok"]"#, "provider:percent"),
        (
            OVERRIDES,
            r#"["claude:80", "anthropic:70"]"#,
            "more than once",
        ),
        ("workflow.provider_limit_max_used_pct", "0", "1..=100"),
        ("workflow.provider_limit_max_used_pct", "101", "1..=100"),
        (
            "workflow.provider_limit_explicit_crews",
            r#""redraw""#,
            "wait, pool",
        ),
    ] {
        let field = key.trim_start_matches("workflow.");
        for error in [
            load(&format!("{field} = {value}")).expect_err(&format!("load {key} = {value}")),
            set(key, value).expect_err(&format!("set {key} = {value}")),
        ] {
            assert!(error.contains(key), "{key} = {value}: {error}");
            assert!(error.contains(expected), "{key} = {value}: {error}");
        }
    }
}

/// [ORB-14699] Budgets are admitted under their canonical provider, sorted,
/// and none are declared by default.
#[test]
fn budgets_are_admitted_under_their_canonical_provider() {
    assert!(
        load("")
            .expect("an empty [workflow] loads")
            .budgets
            .is_empty()
    );
    let policy = load(
        "provider_limit_budgets = [\"xai:30usd/5h\", \"antigravity:200000000tokens/1d\", \
         \"codex:12.5USD/7d\"]",
    )
    .expect("valid budgets load");
    let budgets = policy
        .budgets
        .iter()
        .map(|budget| {
            (
                budget.provider.as_str(),
                budget.amount,
                budget.unit,
                budget.window_minutes(),
                budget.render(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        budgets,
        [
            (
                "antigravity",
                200_000_000.0,
                ProviderLimitBudgetUnit::Tokens,
                24 * 60,
                "antigravity:200000000tokens/1d".to_string()
            ),
            (
                "codex",
                12.5,
                ProviderLimitBudgetUnit::Usd,
                7 * 24 * 60,
                "codex:12.5usd/7d".to_string()
            ),
            (
                "grok",
                30.0,
                ProviderLimitBudgetUnit::Usd,
                5 * 60,
                "grok:30usd/5h".to_string()
            ),
        ],
        "xai is grok; entries are stored sorted and normalized"
    );
    set(
        BUDGETS,
        r#"["grok:30usd/5h", "antigravity:200000000tokens/24h"]"#,
    )
    .expect("config set admits valid budgets");
}

#[test]
fn invalid_budgets_are_refused_by_load_and_config_set() {
    for (value, expected) in [
        (r#"["grok"]"#, "provider:<amount>"),
        (r#"["grok:30usd"]"#, "provider:<amount>"),
        (r#"["grok:30/5h"]"#, "`usd` or `tokens`"),
        (r#"["grok:30eur/5h"]"#, "`usd` or `tokens`"),
        (r#"["nobody:30usd/5h"]"#, "unknown provider 'nobody'"),
        (r#"["grok:0usd/5h"]"#, "positive number of dollars"),
        (r#"["grok:-3usd/5h"]"#, "positive number of dollars"),
        (r#"["grok:1e3usd/5h"]"#, "positive number of dollars"),
        (r#"["grok:usd/5h"]"#, "positive number of dollars"),
        (r#"["grok:1.5tokens/5h"]"#, "whole number of tokens"),
        (r#"["grok:0tokens/5h"]"#, "whole number of tokens"),
        (r#"["grok:30usd/0h"]"#, "`<n>h` or `<n>d`"),
        (r#"["grok:30usd/5m"]"#, "`<n>h` or `<n>d`"),
        (r#"["grok:30usd/5"]"#, "`<n>h` or `<n>d`"),
        (r#"["grok:30usd/h"]"#, "`<n>h` or `<n>d`"),
        (r#"["grok:30usd/32d"]"#, "at most 31 days"),
        (r#"["grok:30usd/5h", "xai:10usd/1d"]"#, "more than once"),
    ] {
        for error in [
            load(&format!("provider_limit_budgets = {value}")).expect_err(&format!("load {value}")),
            set(BUDGETS, value).expect_err(&format!("set {value}")),
        ] {
            assert!(error.contains(BUDGETS), "{value}: {error}");
            assert!(error.contains(expected), "{value}: {error}");
        }
    }
}
