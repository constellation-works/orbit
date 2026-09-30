use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use orbit_types::telemetry::{
    PriceRow, TokenUsage, cost_from_rows, normalize_token_usage_from_rows,
};

const MODEL_PRICES_YAML: &str = include_str!("../../assets/model_prices.yaml");

#[derive(Debug, Deserialize)]
struct PriceTableFile {
    prices: Vec<PriceRow>,
}

// The table is embedded at build time, so a parse failure is a build defect
// the pricing tests catch, never a runtime condition.
#[allow(clippy::expect_used)]
fn price_table() -> &'static [PriceRow] {
    static TABLE: OnceLock<Vec<PriceRow>> = OnceLock::new();
    &TABLE.get_or_init(|| {
        let file: PriceTableFile = serde_yaml::from_str(MODEL_PRICES_YAML)
            .expect("crates/orbit-common/assets/model_prices.yaml must parse");
        file.prices
    })[..]
}

pub fn derive_cost_usd(model: &str, at: DateTime<Utc>, usage: &TokenUsage) -> Option<f64> {
    cost_from_rows(price_table(), model, at, usage)
}

pub fn normalize_token_usage(
    model: &str,
    at: DateTime<Utc>,
    usage: &TokenUsage,
) -> Option<TokenUsage> {
    normalize_token_usage_from_rows(price_table(), model, at, usage)
}

#[cfg(test)]
pub(crate) fn shipped_price_table() -> &'static [PriceRow] {
    price_table()
}
