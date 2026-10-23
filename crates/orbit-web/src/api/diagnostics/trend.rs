//! Clock-aligned trend windows shared by the Analytics › Metrics series.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use super::super::DiagnosticsQuery;
use super::metrics::DiagnosticsRange;

/// Bucket widths a series may use, narrowest first: 5m, 1h, 6h, 1d, 7d.
const BUCKET_LADDER_SECS: [i64; 5] = [300, 3_600, 21_600, 86_400, 604_800];
/// A window takes the narrowest width that splits it into fewer buckets.
/// The floored first bucket can add one partial bucket, so an accepted
/// response carries at most `MAX_BUCKETS + 1` buckets. This ceiling is
/// independent of the caller's date span: an explicit `since`/`month`
/// window no ladder width can split this finely is refused, and an
/// `all` window widens whole weeks past the ladder until it fits.
const MAX_BUCKETS: i64 = 60;
/// Weekly buckets start on Mondays: 1970-01-05 is four days after the epoch.
const WEEK_OFFSET_SECS: i64 = 4 * 86_400;
/// Widest ladder width, used to widen `all` windows in whole weeks.
const WEEK_SECS: i64 = 604_800;

/// Window a trend request without `since` or `month` covers.
const DEFAULT_SINCE: &str = "24h";

/// `since` or `month` as the other diagnostics read them, else the last day.
pub(super) fn trend_range(
    q: &DiagnosticsQuery,
) -> Result<DiagnosticsRange, orbit_core::OrbitError> {
    let range = if q.since.is_some() || q.month.is_some() {
        DiagnosticsRange::from_query(q, false)?
    } else {
        DiagnosticsRange::from_query(
            &DiagnosticsQuery {
                outcome: None,
                since: Some(DEFAULT_SINCE.to_string()),
                month: None,
                limit: None,
            },
            false,
        )?
    };
    validate_trend_range(&range)?;
    Ok(range)
}

/// Refuse an explicit window the bucket ladder cannot bound, or whose
/// equally long previous window is not representable. `all` windows have
/// no previous window and are widened in [`TrendWindow::new`] instead.
fn validate_trend_range(range: &DiagnosticsRange) -> Result<(), orbit_core::OrbitError> {
    let Some(since) = range.since else {
        return Ok(());
    };
    let until = range.until.min(Utc::now());
    let open = since.min(until);
    let span = (until - open).num_seconds().max(1);
    let Some(bucket_secs) = BUCKET_LADDER_SECS
        .iter()
        .copied()
        .find(|width| span / width < MAX_BUCKETS)
    else {
        return Err(orbit_core::OrbitError::InvalidInput(format!(
            "trend window '{}' is too large: it would exceed {MAX_BUCKETS} buckets",
            range.key
        )));
    };
    let start = floor(open, bucket_secs);
    if start.checked_sub_signed(until - start).is_none() {
        return Err(orbit_core::OrbitError::InvalidInput(format!(
            "trend window '{}' is too large: its previous window is not representable",
            range.key
        )));
    }
    Ok(())
}

/// Narrowest ladder width splitting `span` into fewer than
/// [`MAX_BUCKETS`] buckets, or a whole-week multiple past the ladder
/// that does so for an `all` window's unbounded span.
fn bucket_secs_for_span(span: i64) -> i64 {
    if let Some(width) = BUCKET_LADDER_SECS
        .iter()
        .copied()
        .find(|width| span / width < MAX_BUCKETS)
    {
        return width;
    }
    let weeks = span / (MAX_BUCKETS * WEEK_SECS) + 1;
    weeks.saturating_mul(WEEK_SECS)
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
        let bucket_secs = bucket_secs_for_span(span);
        let start = floor(open, bucket_secs);
        // Checked: an explicit window whose previous window is not
        // representable is refused by `trend_range` before this runs;
        // never panic if a caller reaches here without that admission.
        let previous = range.since.and_then(|_| {
            start
                .checked_sub_signed(until - start)
                .map(|previous_start| (previous_start, start))
        });
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
        let offset = Duration::seconds(self.bucket_secs.saturating_mul(index as i64));
        self.start.checked_add_signed(offset).unwrap_or(self.start)
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
    // Whole-week widths (the ladder's widest and its widened multiples)
    // start on Mondays; narrower widths start on the epoch clock.
    let offset = if width % WEEK_SECS == 0 {
        WEEK_OFFSET_SECS
    } else {
        0
    };
    let floored = (at.timestamp() - offset).div_euclid(width) * width + offset;
    DateTime::from_timestamp(floored, 0).unwrap_or(at)
}
