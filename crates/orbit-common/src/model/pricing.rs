use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use orbit_types::telemetry::{
    LONG_PROMPT_CAPTURE_FLOOR_TOKENS, PriceRow, PromptTierRow, TokenUsage, cost_from_rows,
    normalize_token_usage_from_rows,
};

const MODEL_PRICES_YAML: &str = include_str!("../../assets/model_prices.yaml");

#[derive(Debug, Deserialize)]
struct PriceTableFile {
    prices: Vec<PriceRow>,
    #[serde(default)]
    prompt_tiers: Vec<PromptTierRow>,
}

// The table is embedded at build time, so a parse failure is a build defect
// rather than a runtime data condition. So is a tier below the capture floor:
// invocation traces keep no request under it, so such a tier could never
// price the requests it names.
#[allow(clippy::expect_used)]
fn price_table() -> &'static PriceTableFile {
    static TABLE: OnceLock<PriceTableFile> = OnceLock::new();
    TABLE.get_or_init(|| {
        let file: PriceTableFile = serde_yaml::from_str(MODEL_PRICES_YAML)
            .expect("crates/orbit-common/assets/model_prices.yaml must parse");
        if let Some(tier) = file
            .prompt_tiers
            .iter()
            .find(|tier| tier.min_prompt_tokens < LONG_PROMPT_CAPTURE_FLOOR_TOKENS)
        {
            panic!(
                "model_prices.yaml prompt tier for {} starts below the {LONG_PROMPT_CAPTURE_FLOOR_TOKENS}-token capture floor",
                tier.model
            );
        }
        file
    })
}

/// Derive the cost of an invocation from its token totals and, for a model
/// with a prompt-size tier, the per-request usage its trace captured
/// (`InvocationTrace::long_prompt_requests`). `None` when no price row covers
/// the model and date, or when a tier applies and the totals cannot show
/// which requests reached it.
pub fn derive_cost_usd(
    model: &str,
    at: DateTime<Utc>,
    usage: &TokenUsage,
    long_prompt_requests: Option<&[TokenUsage]>,
) -> Option<f64> {
    let table = price_table();
    cost_from_rows(
        &table.prices,
        &table.prompt_tiers,
        model,
        at,
        usage,
        long_prompt_requests,
    )
}

pub fn normalize_token_usage(
    model: &str,
    at: DateTime<Utc>,
    usage: &TokenUsage,
) -> Option<TokenUsage> {
    normalize_token_usage_from_rows(&price_table().prices, model, at, usage)
}
