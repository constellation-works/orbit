use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::policy::PolicyError;

/// Minimum proleptic Gregorian year accepted by persisted policy timestamps,
/// matching `chrono::NaiveDate::MIN` (`-262143-01-01`).
const MIN_YEAR: i32 = (i32::MIN >> 13) + 1;

/// Maximum proleptic Gregorian year accepted by persisted policy timestamps,
/// matching `chrono::NaiveDate::MAX` (`+262142-12-31`).
const MAX_YEAR: i32 = (i32::MAX >> 13) - 1;

const SECS_PER_DAY: i64 = 86_400;
const NANOS_PER_SEC: u32 = 1_000_000_000;

/// Maintained UTC timestamp for persisted [`super::PolicyDef`] and
/// [`crate::resource::PolicyResourceSpec`] records.
///
/// Preserves the wire and ordering contract of `chrono::DateTime<Utc>`:
/// - Deserialization and [`FromStr`] accept relaxed RFC 3339 strings (`T`/`t`/space
///   date-time separator, `Z`/`z`/`UTC` or signed fixed offsets within
///   `-23:59..=+23:59` including U+2212 `−`, unpadded numeric fields, arbitrary
///   fractional digits truncated after 9 nanosecond digits, leap seconds `:60`,
///   and signed extended years in `-262143..=+262142`) and normalize the instant
///   to UTC.
/// - Serialization and [` fmt::Debug`] emit canonical RFC 3339 in UTC with `Z`
///   and automatic subsecond precision (omitted when zero, else 3, 6 or 9 digits).
/// - Ordering and equality compare the normalized UTC instant, preserving leap
///   second representation (`:60` orders after `:59.999999999` and before the
///   next minute's `:00`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PolicyTimestamp {
    year: i32,
    month: u8,
    day: u8,
    secs_of_day: u32,
    frac_nanos: u32,
}

impl PolicyTimestamp {
    /// Current wall-clock time in UTC.
    #[must_use]
    pub fn now_utc() -> Self {
        Self::from(SystemTime::now())
    }

    /// Construct a UTC timestamp from non-leap seconds since `1970-01-01T00:00:00Z`
    /// and nanoseconds (`0..2_000_000_000`, where `1_000_000_000..2_000_000_000`
    /// represents a leap second).
    #[must_use]
    pub const fn from_unix_timestamp(secs: i64, nsecs: u32) -> Option<Self> {
        if nsecs >= 2 * NANOS_PER_SEC {
            return None;
        }
        let days = secs.div_euclid(SECS_PER_DAY);
        let secs_of_day = secs.rem_euclid(SECS_PER_DAY) as u32;
        let Some((year, month, day)) = civil_from_days(days) else {
            return None;
        };
        if year < MIN_YEAR || year > MAX_YEAR {
            return None;
        }
        Some(Self {
            year,
            month,
            day,
            secs_of_day,
            frac_nanos: nsecs,
        })
    }

    /// Non-leap seconds since `1970-01-01T00:00:00Z`.
    #[must_use]
    pub const fn timestamp(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * SECS_PER_DAY + (self.secs_of_day as i64)
    }

    /// Subsecond nanoseconds (`0..2_000_000_000`, exceeding `999_999_999` only on a leap second).
    #[must_use]
    pub const fn timestamp_subsec_nanos(&self) -> u32 {
        self.frac_nanos
    }

    /// Parse a relaxed RFC 3339 timestamp string and normalize it to UTC.
    pub fn parse_rfc3339(input: &str) -> Result<Self, PolicyError> {
        parse_rfc3339_relaxed(input).map_err(|reason| {
            PolicyError::Invalid(format!("invalid RFC 3339 timestamp {input:?}: {reason}"))
        })
    }

    /// Format as canonical UTC RFC 3339 (`YYYY-MM-DDTHH:MM:SS[.fff|.ffffff|.fffffffff]Z`).
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        let mut out = String::with_capacity(30);
        let _ = write_rfc3339_utc(&mut out, self, 'T', "Z");
        out
    }
}

impl fmt::Debug for PolicyTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_rfc3339_utc(f, *self, 'T', "Z")
    }
}

impl fmt::Display for PolicyTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_rfc3339_utc(f, *self, ' ', " UTC")
    }
}

impl FromStr for PolicyTimestamp {
    type Err = PolicyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_rfc3339(s)
    }
}

impl From<SystemTime> for PolicyTimestamp {
    fn from(time: SystemTime) -> Self {
        let (sec, nsec) = match time.duration_since(UNIX_EPOCH) {
            Ok(dur) => (dur.as_secs() as i64, dur.subsec_nanos()),
            Err(err) => {
                let dur = err.duration();
                let (sec, nsec) = (dur.as_secs() as i64, dur.subsec_nanos());
                if nsec == 0 {
                    (-sec, 0)
                } else {
                    (-sec - 1, NANOS_PER_SEC - nsec)
                }
            }
        };
        Self::from_unix_timestamp(sec, nsec).unwrap_or(Self {
            year: 1970,
            month: 1,
            day: 1,
            secs_of_day: 0,
            frac_nanos: 0,
        })
    }
}

impl From<PolicyTimestamp> for SystemTime {
    fn from(ts: PolicyTimestamp) -> Self {
        let sec = ts.timestamp();
        let nsec = ts.timestamp_subsec_nanos();
        if sec < 0 {
            UNIX_EPOCH - Duration::new(sec.unsigned_abs(), 0) + Duration::new(0, nsec)
        } else {
            UNIX_EPOCH + Duration::new(sec as u64, nsec)
        }
    }
}

impl Serialize for PolicyTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        struct FormatIso8601(PolicyTimestamp);

        impl fmt::Display for FormatIso8601 {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_rfc3339_utc(f, self.0, 'T', "Z")
            }
        }

        serializer.collect_str(&FormatIso8601(*self))
    }
}

struct PolicyTimestampVisitor;

impl de::Visitor<'_> for PolicyTimestampVisitor {
    type Value = PolicyTimestamp;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an RFC 3339 formatted date and time string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        PolicyTimestamp::parse_rfc3339(value).map_err(E::custom)
    }
}

impl<'de> Deserialize<'de> for PolicyTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(PolicyTimestampVisitor)
    }
}

fn write_rfc3339_utc(
    w: &mut (impl fmt::Write + ?Sized),
    ts: PolicyTimestamp,
    date_time_sep: char,
    zone_suffix: &str,
) -> fmt::Result {
    let year = ts.year;
    if (0..=9999).contains(&year) {
        write_two(w, (year / 100) as u8)?;
        write_two(w, (year % 100) as u8)?;
    } else {
        write!(w, "{year:+05}")?;
    }
    w.write_char('-')?;
    write_two(w, ts.month)?;
    w.write_char('-')?;
    write_two(w, ts.day)?;
    w.write_char(date_time_sep)?;

    let hour = (ts.secs_of_day / 3600) as u8;
    let minute = ((ts.secs_of_day % 3600) / 60) as u8;
    let mut second = (ts.secs_of_day % 60) as u8;
    let mut nano = ts.frac_nanos;
    if nano >= NANOS_PER_SEC {
        second += 1;
        nano -= NANOS_PER_SEC;
    }

    write_two(w, hour)?;
    w.write_char(':')?;
    write_two(w, minute)?;
    w.write_char(':')?;
    write_two(w, second)?;

    if nano == 0 {
        // Omit fractional part when subsecond is zero (`SecondsFormat::AutoSi`).
    } else if nano.is_multiple_of(1_000_000) {
        write!(w, ".{:03}", nano / 1_000_000)?;
    } else if nano.is_multiple_of(1_000) {
        write!(w, ".{:06}", nano / 1_000)?;
    } else {
        write!(w, ".{nano:09}")?;
    }

    w.write_str(zone_suffix)
}

#[inline]
fn write_two(w: &mut (impl fmt::Write + ?Sized), n: u8) -> fmt::Result {
    w.write_char((b'0' + (n / 10)) as char)?;
    w.write_char((b'0' + (n % 10)) as char)
}

fn parse_rfc3339_relaxed(mut s: &str) -> Result<PolicyTimestamp, &'static str> {
    // 1. Date: year, '-', month, '-', day (with optional leading whitespace before numeric/literal items).
    s = s.trim_start();
    let year: i32 = if let Some(rest) = s.strip_prefix('-') {
        let (rest, raw) = scan_number(rest, 1, usize::MAX)?;
        s = rest;
        let neg = 0i64.checked_sub(raw).ok_or("year out of range")?;
        i32::try_from(neg).map_err(|_| "year out of range")?
    } else if let Some(rest) = s.strip_prefix('+') {
        let (rest, raw) = scan_number(rest, 1, usize::MAX)?;
        s = rest;
        i32::try_from(raw).map_err(|_| "year out of range")?
    } else {
        let (rest, raw) = scan_number(s, 1, 4)?;
        s = rest;
        i32::try_from(raw).map_err(|_| "year out of range")?
    };

    s = s.trim_start();
    s = scan_char(s, b'-')?;
    s = s.trim_start();
    let (rest, month_raw) = scan_number(s, 1, 2)?;
    s = rest;
    if !(1..=12).contains(&month_raw) {
        return Err("month out of range");
    }
    let month = month_raw as u8;

    s = s.trim_start();
    s = scan_char(s, b'-')?;
    s = s.trim_start();
    let (rest, day_raw) = scan_number(s, 1, 2)?;
    s = rest;
    if !(1..=31).contains(&day_raw) {
        return Err("day out of range");
    }
    let day = day_raw as u8;

    // 2. Date-time separator: 't', 'T', or ' ' immediately after day.
    s = match s.as_bytes().first() {
        Some(b't' | b'T' | b' ') => &s[1..],
        Some(_) => return Err("invalid date-time separator"),
        None => return Err("premature end of input"),
    };

    // 3. Time: hour, ':', minute, ':', second, optional '.fraction'.
    s = s.trim_start();
    let (rest, hour_raw) = scan_number(s, 1, 2)?;
    s = rest;
    if !(0..=23).contains(&hour_raw) {
        return Err("hour out of range");
    }
    let hour = hour_raw as u32;

    s = s.trim_start();
    s = scan_char(s, b':')?;
    s = s.trim_start();
    let (rest, minute_raw) = scan_number(s, 1, 2)?;
    s = rest;
    if !(0..=59).contains(&minute_raw) {
        return Err("minute out of range");
    }
    let minute = minute_raw as u32;

    s = s.trim_start();
    s = scan_char(s, b':')?;
    s = s.trim_start();
    let (rest, second_raw) = scan_number(s, 1, 2)?;
    s = rest;
    if !(0..=60).contains(&second_raw) {
        return Err("second out of range");
    }
    let second = second_raw as u32;

    let subsec_nanos = if let Some(after_dot) = s.strip_prefix('.') {
        let (rest, nanos) = scan_nanosecond(after_dot)?;
        s = rest;
        nanos
    } else {
        0
    };

    // 4. Timezone offset: 'UTC' (case-insensitive), 'Z'/'z', or signed offset.
    s = s.trim_start();
    let (rest, offset_secs) = if s.len() >= 3 && s.as_bytes()[..3].eq_ignore_ascii_case(b"UTC") {
        (&s[3..], 0)
    } else {
        scan_timezone_offset(s)?
    };
    s = rest;

    if !s.trim_start().is_empty() {
        return Err("trailing input");
    }

    // 5. Validate calendar date and convert local time + offset to UTC.
    if !is_valid_ymd(year, month, day) {
        return Err("date out of range");
    }
    if !(-86_399..=86_399).contains(&offset_secs) {
        return Err("timezone offset out of range");
    }

    let (sec, leap_nanos) = if second == 60 {
        (59, NANOS_PER_SEC)
    } else {
        (second, 0)
    };
    let frac_nanos = leap_nanos + subsec_nanos;
    if frac_nanos >= 2 * NANOS_PER_SEC {
        return Err("nanosecond out of range");
    }

    let local_secs = (hour * 3600 + minute * 60 + sec) as i32;
    let utc_secs_unwrapped = local_secs - offset_secs;
    let day_delta = utc_secs_unwrapped.div_euclid(86_400);
    let secs_of_day = utc_secs_unwrapped.rem_euclid(86_400) as u32;

    let (utc_year, utc_month, utc_day) = match day_delta {
        -1 => pred_ymd(year, month, day).ok_or("UTC date out of range")?,
        0 => (year, month, day),
        1 => succ_ymd(year, month, day).ok_or("UTC date out of range")?,
        _ => return Err("UTC date out of range"),
    };

    Ok(PolicyTimestamp {
        year: utc_year,
        month: utc_month,
        day: utc_day,
        secs_of_day,
        frac_nanos,
    })
}

#[inline]
fn scan_char(s: &str, expected: u8) -> Result<&str, &'static str> {
    match s.as_bytes().first() {
        Some(&byte) if byte == expected => Ok(&s[1..]),
        Some(_) => Err("unexpected character"),
        None => Err("premature end of input"),
    }
}

#[inline]
fn scan_number(s: &str, min: usize, max: usize) -> Result<(&str, i64), &'static str> {
    let bytes = s.as_bytes();
    if bytes.len() < min {
        return Err("premature end of input");
    }
    let mut value = 0i64;
    for (index, &byte) in bytes.iter().take(max).enumerate() {
        if !byte.is_ascii_digit() {
            if index < min {
                return Err("invalid digit");
            }
            return Ok((&s[index..], value));
        }
        value = value
            .checked_mul(10)
            .and_then(|acc| acc.checked_add(i64::from(byte - b'0')))
            .ok_or("number out of range")?;
    }
    let consumed = max.min(bytes.len());
    Ok((&s[consumed..], value))
}

#[inline]
fn scan_nanosecond(s: &str) -> Result<(&str, u32), &'static str> {
    let orig_len = s.len();
    let (rest, raw) = scan_number(s, 1, 9)?;
    let consumed = orig_len - rest.len();
    const SCALE: [u32; 10] = [
        0,
        100_000_000,
        10_000_000,
        1_000_000,
        100_000,
        10_000,
        1_000,
        100,
        10,
        1,
    ];
    let nanos = (raw as u32)
        .checked_mul(SCALE[consumed])
        .ok_or("nanosecond out of range")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    Ok((rest, nanos))
}

fn scan_timezone_offset(mut s: &str) -> Result<(&str, i32), &'static str> {
    if let Some(b'Z' | b'z') = s.as_bytes().first() {
        return Ok((&s[1..], 0));
    }
    let negative = match s.chars().next() {
        Some('+') => {
            s = &s['+'.len_utf8()..];
            false
        }
        Some('-') => {
            s = &s['-'.len_utf8()..];
            true
        }
        Some('−') => {
            s = &s['−'.len_utf8()..];
            true
        }
        Some(_) => return Err("invalid timezone offset sign"),
        None => return Err("premature end of input"),
    };

    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return Err("premature end of input");
    }
    let hours = match (bytes[0], bytes[1]) {
        (h1 @ b'0'..=b'9', h2 @ b'0'..=b'9') => i32::from((h1 - b'0') * 10 + (h2 - b'0')),
        _ => return Err("invalid timezone offset hours"),
    };
    s = &s[2..];

    s = s.trim_start_matches(|c: char| c == ':' || c.is_whitespace());

    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return Err("premature end of input");
    }
    let minutes = match (bytes[0], bytes[1]) {
        (m1 @ b'0'..=b'5', m2 @ b'0'..=b'9') => i32::from((m1 - b'0') * 10 + (m2 - b'0')),
        (b'6'..=b'9', b'0'..=b'9') => return Err("timezone offset minutes out of range"),
        _ => return Err("invalid timezone offset minutes"),
    };
    s = &s[2..];

    let total_secs = hours * 3600 + minutes * 60;
    Ok((s, if negative { -total_secs } else { total_secs }))
}

#[inline]
const fn is_leap_year(year: i32) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

#[inline]
const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

#[inline]
const fn is_valid_ymd(year: i32, month: u8, day: u8) -> bool {
    if year < MIN_YEAR || year > MAX_YEAR {
        return false;
    }
    let max_day = days_in_month(year, month);
    max_day != 0 && day >= 1 && day <= max_day
}

#[inline]
const fn pred_ymd(year: i32, month: u8, day: u8) -> Option<(i32, u8, u8)> {
    if day > 1 {
        Some((year, month, day - 1))
    } else if month > 1 {
        let prev_month = month - 1;
        Some((year, prev_month, days_in_month(year, prev_month)))
    } else if year > MIN_YEAR {
        Some((year - 1, 12, 31))
    } else {
        None
    }
}

#[inline]
const fn succ_ymd(year: i32, month: u8, day: u8) -> Option<(i32, u8, u8)> {
    if day < days_in_month(year, month) {
        Some((year, month, day + 1))
    } else if month < 12 {
        Some((year, month + 1, 1))
    } else if year < MAX_YEAR {
        Some((year + 1, 1, 1))
    } else {
        None
    }
}

#[inline]
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = (year as i64) - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u32;
    let m = month as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (day as u32) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

#[inline]
const fn civil_from_days(days: i64) -> Option<(i32, u8, u8)> {
    let Some(z) = days.checked_add(719_468) else {
        return None;
    };
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let Some(era_years) = era.checked_mul(400) else {
        return None;
    };
    let Some(y) = (yoe as i64).checked_add(era_years) else {
        return None;
    };
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let Some(year_i64) = y.checked_add(if m <= 2 { 1 } else { 0 }) else {
        return None;
    };
    if year_i64 < (i32::MIN as i64) || year_i64 > (i32::MAX as i64) {
        return None;
    }
    Some((year_i64 as i32, m as u8, d as u8))
}
