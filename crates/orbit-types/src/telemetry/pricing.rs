//! Versioned model price table (ORB-10338, ADR-0245).
//!
//! Cost is derived from token splits at query time against a price row keyed
//! by exact model string and effective date range, rather than computed once
//! and frozen at ingest. The table itself is a data asset
//! (`assets/model_prices.yaml`), not Rust — adding or correcting a rate is a
//! YAML edit, no code change required. See ADR-0245 for the tradeoffs.
//! Prompt-size tiers ([`PromptTierRow`]) price one model request by its own
//! prompt, from the per-request usage an invocation trace captured.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::invocation::TokenUsage;

/// How a provider reports the input-token total carried by [`TokenUsage`].
///
/// Most providers report mutually exclusive input and cache buckets. OpenAI
/// reports a gross input total that already includes cached reads and writes,
/// so those buckets must be removed before the full input rate is applied.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputTokenBasis {
    /// `TokenUsage::input` excludes every cache bucket.
    #[default]
    Exclusive,
    /// `TokenUsage::input` includes cache reads and both cache-write buckets.
    GrossIncludesCache,
}

/// One versioned price row: USD per million tokens, by exact model string
/// and the date range this rate was in effect. `effective_until` is
/// exclusive; `None` means the rate is still current.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PriceRow {
    pub model: String,
    pub effective_from: DateTime<Utc>,
    #[serde(default)]
    pub effective_until: Option<DateTime<Utc>>,
    /// Provider reporting convention for [`TokenUsage::input`]. Existing rows
    /// default to [`InputTokenBasis::Exclusive`] for backward compatibility.
    #[serde(default)]
    pub input_token_basis: InputTokenBasis,
    pub input_per_million_usd: f64,
    pub cache_read_per_million_usd: f64,
    /// Rate for standard 5-minute-TTL cache-creation (write) tokens
    /// ([`TokenUsage::cache_create`]).
    pub cache_create_per_million_usd: f64,
    /// Rate for premium 1-hour-TTL cache-creation (write) tokens
    /// ([`TokenUsage::cache_create_1h`]). Defaults to `0.0` for providers with
    /// no 1h-TTL concept (their `cache_create_1h` count is always zero, so the
    /// rate is never applied); Anthropic rows set it to 2x the input rate.
    #[serde(default)]
    pub cache_create_1h_per_million_usd: f64,
    pub output_per_million_usd: f64,
}

impl PriceRow {
    fn covers(&self, model: &str, at: DateTime<Utc>) -> bool {
        self.model == model
            && at >= self.effective_from
            && self.effective_until.is_none_or(|until| at < until)
    }

    fn cost_usd(&self, usage: &TokenUsage) -> Option<f64> {
        self.rates().cost_usd(self.input_token_basis, usage)
    }

    fn rates(&self) -> Rates {
        Rates {
            input: self.input_per_million_usd,
            cache_read: self.cache_read_per_million_usd,
            cache_create: self.cache_create_per_million_usd,
            cache_create_1h: self.cache_create_1h_per_million_usd,
            output: self.output_per_million_usd,
        }
    }
}

/// Higher rates for one model request whose own prompt reaches
/// `min_prompt_tokens`, by exact model string and effective date range like
/// [`PriceRow`]. The tier prices the whole request, every bucket at its tier
/// rate, and leaves requests below the threshold at the base row's rates.
///
/// A request's prompt counts every input-side token after the base row's
/// [`InputTokenBasis`] is applied: uncached input, cache reads and both
/// cache-write buckets. The threshold is inclusive, so "over 100,000 tokens"
/// is `100001` and "200,000 or more" is `200000`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PromptTierRow {
    pub model: String,
    pub effective_from: DateTime<Utc>,
    #[serde(default)]
    pub effective_until: Option<DateTime<Utc>>,
    pub min_prompt_tokens: u64,
    pub input_per_million_usd: f64,
    pub cache_read_per_million_usd: f64,
    pub cache_create_per_million_usd: f64,
    pub cache_create_1h_per_million_usd: f64,
    pub output_per_million_usd: f64,
}

impl PromptTierRow {
    fn covers(&self, model: &str, at: DateTime<Utc>) -> bool {
        self.model == model
            && at >= self.effective_from
            && self.effective_until.is_none_or(|until| at < until)
    }

    fn rates(&self) -> Rates {
        Rates {
            input: self.input_per_million_usd,
            cache_read: self.cache_read_per_million_usd,
            cache_create: self.cache_create_per_million_usd,
            cache_create_1h: self.cache_create_1h_per_million_usd,
            output: self.output_per_million_usd,
        }
    }
}

/// USD per million tokens for each [`TokenUsage`] bucket.
struct Rates {
    input: f64,
    cache_read: f64,
    cache_create: f64,
    cache_create_1h: f64,
    output: f64,
}

impl Rates {
    fn cost_usd(&self, basis: InputTokenBasis, usage: &TokenUsage) -> Option<f64> {
        let input = exclusive_input(basis, usage)?;
        Some(
            rate(input, self.input)
                + rate(usage.cache_read, self.cache_read)
                + rate(usage.cache_create, self.cache_create)
                + rate(usage.cache_create_1h, self.cache_create_1h)
                + rate(usage.output, self.output),
        )
    }
}

/// `usage.input` without any cache bucket. Checked subtraction makes an
/// inconsistent gross total unknown instead of silently saturating.
fn exclusive_input(basis: InputTokenBasis, usage: &TokenUsage) -> Option<u64> {
    match basis {
        InputTokenBasis::Exclusive => Some(usage.input),
        InputTokenBasis::GrossIncludesCache => usage.input.checked_sub(
            usage
                .cache_read
                .checked_add(usage.cache_create)?
                .checked_add(usage.cache_create_1h)?,
        ),
    }
}

/// The prompt a [`PromptTierRow`] threshold compares: every input-side token.
fn prompt_tokens(basis: InputTokenBasis, usage: &TokenUsage) -> Option<u64> {
    exclusive_input(basis, usage)?
        .checked_add(usage.cache_read)?
        .checked_add(usage.cache_create)?
        .checked_add(usage.cache_create_1h)
}

/// Strip a trailing context-window marker like `[1m]` / `[200k]` from a model
/// string, returning the base model key. The fleet's ledger records the 1M
/// context variant of Opus as `claude-opus-4-8[1m]` (a `modelUsage` key), but
/// those runs are billed at the same per-token rates as the base model — the
/// bracket denotes the context window, not a distinct price tier. Rather than
/// duplicate every row for each suffix, [`cost_from_rows`] falls back to the
/// stripped key when no exact row matches. A premium for one request's prompt
/// size is a [`PromptTierRow`] on the base key; a premium for the whole
/// context-window variant would be a row keyed by the full `model[1m]`
/// string, which wins via the exact-match pass.
fn strip_context_suffix(model: &str) -> Option<&str> {
    if model.ends_with(']') {
        model
            .rfind('[')
            .map(|open| model[..open].trim_end())
            .filter(|base| !base.is_empty())
    } else {
        None
    }
}

fn rate(tokens: u64, per_million_usd: f64) -> f64 {
    (tokens as f64 / 1_000_000.0) * per_million_usd
}

/// The covering row with the latest `effective_from`: an exact model match
/// first (so a dedicated `model[1m]` premium row would win), then the base key
/// with any context-window suffix stripped.
fn pick_row<'a>(rows: &'a [PriceRow], model: &str, at: DateTime<Utc>) -> Option<&'a PriceRow> {
    let pick = |model: &str| {
        rows.iter()
            .filter(|row| row.covers(model, at))
            .max_by(|a, b| a.effective_from.cmp(&b.effective_from))
    };
    pick(model).or_else(|| strip_context_suffix(model).and_then(pick))
}

/// Price `usage` against the covering row and, when a [`PromptTierRow`]
/// covers that row's model, its prompt-size tier.
///
/// The invocation totals are priced at the base row's rates. Each request in
/// `long_prompt_requests` whose own prompt reaches the tier then adds the
/// difference between its tier and base cost. Without per-request usage
/// (`None`), the totals bound every request's prompt from above: below the
/// threshold no request can reach the tier and the base cost is exact; at or
/// above it the cost is unknown, never estimated from the totals.
pub fn cost_from_rows(
    rows: &[PriceRow],
    tiers: &[PromptTierRow],
    model: &str,
    at: DateTime<Utc>,
    usage: &TokenUsage,
    long_prompt_requests: Option<&[TokenUsage]>,
) -> Option<f64> {
    let row = pick_row(rows, model, at)?;
    let base = row.cost_usd(usage)?;
    let Some(tier) = tiers
        .iter()
        .filter(|tier| tier.covers(&row.model, at))
        .max_by(|a, b| a.effective_from.cmp(&b.effective_from))
    else {
        return Some(base);
    };
    let Some(requests) = long_prompt_requests else {
        return (prompt_tokens(row.input_token_basis, usage)? < tier.min_prompt_tokens)
            .then_some(base);
    };
    let tier_rates = tier.rates();
    let mut premium = 0.0;
    for request in requests {
        if prompt_tokens(row.input_token_basis, request)? >= tier.min_prompt_tokens {
            premium +=
                tier_rates.cost_usd(row.input_token_basis, request)? - row.cost_usd(request)?;
        }
    }
    Some(base + premium)
}

/// Converts provider-reported usage into mutually-exclusive token buckets
/// using an already-selected price table.
pub fn normalize_token_usage_from_rows(
    rows: &[PriceRow],
    model: &str,
    at: DateTime<Utc>,
    usage: &TokenUsage,
) -> Option<TokenUsage> {
    let row = pick_row(rows, model, at)?;
    Some(TokenUsage {
        input: exclusive_input(row.input_token_basis, usage)?,
        ..usage.clone()
    })
}
