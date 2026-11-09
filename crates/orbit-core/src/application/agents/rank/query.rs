use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::query::{SpanError, parse_span};
use super::super::{AgentFacts, AgentQuery, WindowBounds, WindowSpan};

const DEFAULT_SETTLE_LAG: &str = "14d";

/// A validated ranking selection.
#[derive(Debug, Clone)]
pub struct RankQuery {
    /// The ranked window, ending at the settled cutoff (or at `until` when
    /// live).
    pub(super) query: AgentQuery,
    until: DateTime<Utc>,
    settle_lag: String,
    pub(super) live: bool,
}

impl RankQuery {
    /// Validate a selection. `window` is a time window (default `30d`);
    /// `until` an RFC 3339 instant or a duration ago (default now);
    /// `settle_lag` a duration (default `14d`) the ranked window ends
    /// before `until`, unless `live`; `execution` the scorecard's selector.
    pub fn parse(
        window: Option<&str>,
        until: Option<&str>,
        settle_lag: Option<&str>,
        execution: Option<&str>,
        live: bool,
    ) -> Result<Self, OrbitError> {
        let query = AgentQuery::parse(window, None)?
            .with_execution(execution)?
            .with_until(until)?;
        if matches!(query.span, WindowSpan::Attempts(_)) {
            return Err(OrbitError::InvalidInput(format!(
                "the ranking needs a time window such as 30d; got '{}'",
                query.requested
            )));
        }
        let settle_lag = settle_lag
            .map(str::trim)
            .unwrap_or(DEFAULT_SETTLE_LAG)
            .to_string();
        let lag = parse_span(&settle_lag).map_err(|error| {
            OrbitError::InvalidInput(match error {
                SpanError::Malformed => {
                    format!("settle_lag must be a duration such as 14d; got '{settle_lag}'")
                }
                SpanError::OutOfRange => format!("settle_lag is out of range: {settle_lag}"),
            })
        })?;
        let until = query.until.unwrap_or_else(Utc::now);
        let cutoff = if live {
            until
        } else {
            until.checked_sub_signed(lag).ok_or_else(|| {
                OrbitError::InvalidInput(format!("settle_lag is out of range: {settle_lag}"))
            })?
        };
        Ok(Self {
            query: query.with_until_at(cutoff),
            until,
            settle_lag,
            live,
        })
    }

    pub(super) fn header(
        &self,
        facts: &AgentFacts,
        window: WindowBounds,
        cache_ttl: Duration,
    ) -> Value {
        let mut header = self.query.header(facts, window, cache_ttl);
        if let Some(fields) = header.as_object_mut() {
            fields.remove("min_support");
            fields.remove("window_attempts");
        }
        header["role"] = json!("implementers");
        header["requested_until"] = json!(self.until.to_rfc3339_opts(SecondsFormat::Secs, true));
        header["settle_lag"] = json!(self.settle_lag);
        header["live"] = json!(self.live);
        // A live board's newest work has not had time to show its escaped
        // regressions; it is never frozen or published.
        header["provisional"] = json!(self.live);
        header
    }
}
