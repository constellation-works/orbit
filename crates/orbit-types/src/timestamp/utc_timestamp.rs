use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use time::{Date, Duration, Month, UtcDateTime, UtcOffset};

use super::UtcTimestampError;

// Chrono's persisted date limits. The backend supports both endpoints and
// timezone normalization across either boundary.
const LEGACY_UTC_YEARS: std::ops::RangeInclusive<i32> = -262_143..=262_142;

/// A UTC timestamp for persisted records that use Orbit's legacy date format.
///
/// Reads the legacy relaxed RFC 3339 wire format, including signed extended
/// years, fixed minute offsets, and leap seconds. Writes UTC with a `Z`
/// suffix and 0, 3, 6, or 9 fractional digits. Leap seconds sort after the
/// preceding ordinary second and before the following minute.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct UtcTimestamp {
    utc: UtcDateTime,
    // The backend stores second 59 and its fraction. The marker preserves
    // legacy :60 across offset conversion, formatting, and comparisons.
    leap_second: bool,
}

impl Ord for UtcTimestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.utc
            .unix_timestamp()
            .cmp(&other.utc.unix_timestamp())
            .then_with(|| self.leap_second.cmp(&other.leap_second))
            .then_with(|| self.utc.nanosecond().cmp(&other.utc.nanosecond()))
    }
}

impl PartialOrd for UtcTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<SystemTime> for UtcTimestamp {
    type Error = UtcTimestampError;

    fn try_from(value: SystemTime) -> Result<Self, Self::Error> {
        let epoch = UtcDateTime::UNIX_EPOCH;
        let utc = match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Duration::try_from(duration)
                .ok()
                .and_then(|duration| epoch.checked_add(duration)),
            Err(error) => Duration::try_from(error.duration())
                .ok()
                .and_then(|duration| epoch.checked_sub(duration)),
        };
        utc.filter(|utc| LEGACY_UTC_YEARS.contains(&utc.year()))
            .map(|utc| Self {
                utc,
                leap_second: false,
            })
            .ok_or(UtcTimestampError::OutOfRange)
    }
}

impl FromStr for UtcTimestamp {
    type Err = UtcTimestampError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        parse_legacy_timestamp(input).ok_or_else(|| UtcTimestampError::Invalid(input.into()))
    }
}

impl UtcTimestamp {
    fn format_utc(&self, f: &mut fmt::Formatter<'_>, separator: char, suffix: &str) -> fmt::Result {
        let (year, month, day) = self.utc.date().to_calendar_date();
        let (hour, minute, second, nanos) = self.utc.time().as_hms_nano();
        let second = if self.leap_second { 60 } else { second };
        if (0..=9999).contains(&year) {
            write!(f, "{year:04}")?;
        } else {
            write!(f, "{year:+05}")?;
        }
        write!(
            f,
            "-{:02}-{day:02}{separator}{hour:02}:{minute:02}:{second:02}",
            u8::from(month)
        )?;
        if nanos != 0 {
            if nanos.is_multiple_of(1_000_000) {
                write!(f, ".{:03}", nanos / 1_000_000)?;
            } else if nanos.is_multiple_of(1_000) {
                write!(f, ".{:06}", nanos / 1_000)?;
            } else {
                write!(f, ".{nanos:09}")?;
            }
        }
        f.write_str(suffix)
    }
}

impl fmt::Debug for UtcTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format_utc(f, 'T', "Z")
    }
}

impl fmt::Display for UtcTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format_utc(f, ' ', " UTC")
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct WireTimestamp<'a>(&'a UtcTimestamp);

        impl fmt::Display for WireTimestamp<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.format_utc(f, 'T', "Z")
            }
        }

        serializer.collect_str(&WireTimestamp(self))
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_legacy_utc_timestamp(deserializer)
    }
}

pub(crate) fn deserialize_legacy_utc_timestamp<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: fmt::Display,
{
    struct TimestampVisitor<T>(std::marker::PhantomData<T>);

    impl<T: FromStr> de::Visitor<'_> for TimestampVisitor<T>
    where
        T::Err: fmt::Display,
    {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an RFC 3339 formatted date and time string")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            value.parse().map_err(E::custom)
        }
    }

    deserializer.deserialize_str(TimestampVisitor(std::marker::PhantomData))
}

// Read the legacy wire spelling before handing component validation and
// instant normalization to the backend.
fn parse_legacy_timestamp(mut input: &str) -> Option<UtcTimestamp> {
    input = input.trim_start();
    let (sign, width) = match input.as_bytes().first()? {
        b'-' => {
            input = &input[1..];
            (-1, usize::MAX)
        }
        b'+' => {
            input = &input[1..];
            (1, usize::MAX)
        }
        _ => (1, 4),
    };
    let year = sign * take_digits(&mut input, width).parse::<i32>().ok()?;
    if !LEGACY_UTC_YEARS.contains(&year) {
        return None;
    }

    input = input.trim_start().strip_prefix('-')?.trim_start();
    let month = Month::try_from(take_digits(&mut input, 2).parse::<u8>().ok()?).ok()?;
    input = input.trim_start().strip_prefix('-')?.trim_start();
    let day = take_digits(&mut input, 2).parse().ok()?;
    let date = Date::from_calendar_date(year, month, day).ok()?;

    input = input.strip_prefix(['T', 't', ' '])?.trim_start();
    let hour = take_digits(&mut input, 2).parse().ok()?;
    input = input.trim_start().strip_prefix(':')?.trim_start();
    let minute = take_digits(&mut input, 2).parse().ok()?;
    input = input.trim_start().strip_prefix(':')?.trim_start();
    let second = take_digits(&mut input, 2).parse::<u8>().ok()?;
    let leap_second = second == 60;

    let mut nanos = 0;
    if let Some(fraction) = input.strip_prefix('.') {
        input = fraction;
        let digits = take_digits(&mut input, usize::MAX);
        let retained = &digits[..digits.len().min(9)];
        nanos = retained.parse::<u32>().ok()? * 10_u32.pow(9 - retained.len() as u32);
    }
    let offset = parse_legacy_offset(input.trim())?;
    let utc = date
        .with_hms_nano(hour, minute, if leap_second { 59 } else { second }, nanos)
        .ok()?
        .assume_offset(offset)
        .to_utc();
    LEGACY_UTC_YEARS
        .contains(&utc.year())
        .then_some(UtcTimestamp { utc, leap_second })
}

fn take_digits<'a>(input: &mut &'a str, width: usize) -> &'a str {
    let count = input
        .bytes()
        .take(width)
        .take_while(u8::is_ascii_digit)
        .count();
    let (digits, rest) = input.split_at(count);
    *input = rest;
    digits
}

fn parse_legacy_offset(mut input: &str) -> Option<UtcOffset> {
    if input.eq_ignore_ascii_case("Z") || input.eq_ignore_ascii_case("UTC") {
        return Some(UtcOffset::UTC);
    }
    let sign = if let Some(rest) = input.strip_prefix('+') {
        input = rest;
        1
    } else {
        input = input.strip_prefix(['-', '−'])?;
        -1
    };

    let hours = take_digits(&mut input, 2);
    let minutes = input.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if hours.len() != 2 || minutes.len() != 2 || !minutes.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let hours = hours.parse::<i8>().ok()?;
    // The legacy wire format accepts offsets only through ±23:59.
    if hours > 23 {
        return None;
    }
    UtcOffset::from_hms(sign * hours, sign * minutes.parse::<i8>().ok()?, 0).ok()
}
