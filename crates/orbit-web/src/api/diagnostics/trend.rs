//! Clock-aligned trend windows shared by the Analytics › Metrics series.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use super::super::DiagnosticsQuery;
use super::metrics::DiagnosticsRange;

/// Bucket widths a series may use, narrowest first: 5m, 1h, 6h, 1d, 7d.
const BUCKET_LADDER_SECS: [i64; 5] = [300, 3_600, 21_600, 86_400, 604_800];
/// A window takes the narrowest width that splits it into fewer buckets.
const MAX_BUCKETS: i64 = 60;
/// Weekly buckets start on Mondays: 1970-01-05 is four days after the epoch.
const WEEK_OFFSET_SECS: i64 = 4 * 86_400;

/// Window a trend request without `since` or `month` covers.
const DEFAULT_SINCE: &str = "24h";

/// `since` or `month` as the other diagnostics read them, else the last day.
pub(super) fn trend_range(
    q: &DiagnosticsQuery,
) -> Result<DiagnosticsRange, orbit_core::OrbitError> {
    if q.since.is_some() || q.month.is_some() {
        return DiagnosticsRange::from_query(q, false);
    }
    DiagnosticsRange::from_query(
        &DiagnosticsQuery {
            outcome: None,
            since: Some(DEFAULT_SINCE.to_string()),
            month: None,
            limit: None,
        },
        false,
    )
}

/// The buckets a series is cut into, with the equally long window before it.
///
/// The requested `since` is floored to the bucket width, so every bucket but
/// the last is complete and starts on the clock (`:00`, `00:00 UTC`, …). The
/// last bucket is the one `until` falls in.
pub(super) struct TrendWindow {
    key: String,
    pub(super) start: DateTime<Utc>,
    pub(super) until: DateTime<Utc>,
    bucket_secs: i64,
    /// `[start - (until - start), start)`; `None` for an `all` window.
    pub(super) previous: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

impl TrendWindow {
    /// An `all` range (no `since`) opens at `earliest`, the oldest sample the
    /// caller read, or at `until` when there is none.
    pub(super) fn new(range: &DiagnosticsRange, earliest: Option<DateTime<Utc>>) -> Self {
        let until = range.until.min(Utc::now());
        let open = range.since.or(earliest).unwrap_or(until).min(until);
        let span = (until - open).num_seconds().max(1);
        let bucket_secs = BUCKET_LADDER_SECS
            .iter()
            .copied()
            .find(|width| span / width < MAX_BUCKETS)
            .unwrap_or(BUCKET_LADDER_SECS[BUCKET_LADDER_SECS.len() - 1]);
        let start = floor(open, bucket_secs);
        let previous = range.since.map(|_| (start - (until - start), start));
        Self {
            key: range.key.clone(),
            start,
            until,
            bucket_secs,
            previous,
        }
    }

    /// The oldest instant a caller must read to fill this window and the
    /// previous one; `None` reads everything.
    pub(super) fn read_since(range: &DiagnosticsRange) -> Option<DateTime<Utc>> {
        range.since.map(|_| {
            let window = Self::new(range, None);
            window.previous.map_or(window.start, |(start, _)| start)
        })
    }

    pub(super) fn bucket_count(&self) -> usize {
        let last = floor(self.until, self.bucket_secs);
        usize::try_from((last - self.start).num_seconds() / self.bucket_secs + 1).unwrap_or(1)
    }

    /// The bucket `at` falls in, or `None` outside `[start, until]`.
    pub(super) fn bucket_of(&self, at: DateTime<Utc>) -> Option<usize> {
        if at < self.start || at > self.until {
            return None;
        }
        let index = (at - self.start).num_seconds() / self.bucket_secs;
        usize::try_from(index)
            .ok()
            .map(|index| index.min(self.bucket_count() - 1))
    }

    pub(super) fn in_previous(&self, at: DateTime<Utc>) -> bool {
        self.previous
            .is_some_and(|(start, end)| at >= start && at < end)
    }

    pub(super) fn bucket_start(&self, index: usize) -> DateTime<Utc> {
        self.start + Duration::seconds(self.bucket_secs * index as i64)
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "since": self.start.to_rfc3339(),
            "until": self.until.to_rfc3339(),
            "bucket_seconds": self.bucket_secs,
            "previous_since": self.previous.map(|(start, _)| start.to_rfc3339()),
            "previous_until": self.previous.map(|(_, end)| end.to_rfc3339()),
        })
    }
}

fn floor(at: DateTime<Utc>, width: i64) -> DateTime<Utc> {
    let offset = if width == 604_800 {
        WEEK_OFFSET_SECS
    } else {
        0
    };
    let floored = (at.timestamp() - offset).div_euclid(width) * width + offset;
    DateTime::from_timestamp(floored, 0).unwrap_or(at)
}
