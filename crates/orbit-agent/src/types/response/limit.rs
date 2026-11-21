//! Provider usage-limit text [ORB-14695]: whether a provider's own failure
//! says its account hit a usage limit, and what it said about the limit.
//!
//! Recorded texts, from each CLI's binary or a live failure:
//!
//! - codex-cli 0.161.0: `You've hit your usage limit[ for <model>]. … Try
//!   again at <time>.`, `Usage limit reached. You've reached your usage
//!   limit.` and `You're out of credits.` `<time>` is the host's local time,
//!   `3:42 PM` on the day of the failure, else `Oct 9th, 2026 3:42 PM`.
//! - Claude Code 2.1.294: messages that start `You've hit your`, `You've
//!   reached your` or `You're out of usage credits`, such as `You've hit your
//!   limit · resets 3pm (America/Los_Angeles)`.
//! - Antigravity: `Individual quota reached. … Resets in 1h37m37s.`
//! - Gemini CLI: `TerminalQuotaError: You exceeded your current quota`.
//! - Muse: `Subscription quota exhausted … resets at <RFC3339>`.
//! - grok 1.0.46: HTTP `Too Many Requests (` and `Payment Required (`; Grok
//!   Build: `API error (status 402 Payment Required): Grok Build usage balance
//!   exhausted`.

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta,
    TimeZone, Timelike, Utc,
};
use orbit_types::workflow::ProviderLimitFailure;
use tz::{TimeZoneRef, datetime::FoundDateTimeKind};

/// Phrases that alone say a usage limit was reached. Lowercase, with
/// typographic apostrophes folded to `'`.
const PROVIDER_LIMIT_PHRASES: &[&str] = &[
    "you've hit your usage limit",
    "you've reached your usage limit",
    "usage limit reached",
    "usage_limit_reached",
    "you're out of credits",
    "you're out of usage credits",
    "individual quota reached",
    "terminalquotaerror",
    "exceeded your current quota",
    "too many requests (",
    "payment required (",
    "payment required)",
    "usage balance exhausted",
    "subscription quota exhausted",
];

/// Claude Code's openings, which say a limit only alongside the word.
const PROVIDER_LIMIT_OPENINGS: &[&str] = &["you've hit your", "you've reached your"];

fn fold(text: &str) -> String {
    text.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

/// Whether a provider's own failure text says its account hit a usage limit,
/// so neither a repair agent nor a rerun on that account can succeed before
/// the limit resets.
///
/// Pass only text the provider wrote about itself, as for
/// [`super::provider_authentication_failure`]: an agent quoting a GitHub rate
/// limit in its transcript or envelope is not the provider's limit.
#[must_use]
pub fn provider_usage_limit(text: &str) -> bool {
    let text = fold(text);
    PROVIDER_LIMIT_PHRASES
        .iter()
        .any(|phrase| text.contains(phrase))
        || (PROVIDER_LIMIT_OPENINGS
            .iter()
            .any(|opening| text.contains(opening))
            && text.contains("limit"))
}

/// What a provider's limit text says about the limit: the model it names and
/// when it resets. `now` is when the failure was seen, and `local` the
/// host's UTC offset, which codex's `Try again at` uses. A part the text
/// does not state stays `None`.
#[must_use]
pub fn provider_usage_limit_details(
    text: &str,
    now: DateTime<Utc>,
    local: FixedOffset,
) -> ProviderLimitFailure {
    let folded = fold(text);
    ProviderLimitFailure {
        model: limit_model(&folded),
        window: None,
        resets_at: relative_reset(&folded, now)
            .or_else(|| rfc3339_reset(text))
            .or_else(|| try_again_at(&folded, now, local))
            .or_else(|| resets_at_zone(&folded, now))
            .or_else(|| epoch_suffix(&folded)),
    }
}

/// Muse's `resets at <RFC3339>` timestamp, with optional trailing punctuation.
fn rfc3339_reset(text: &str) -> Option<DateTime<Utc>> {
    const LEAD: &str = "resets at ";
    // ASCII case folding preserves byte positions for the timestamp only when
    // the prefix is ASCII, so locate it in the original text instead.
    let start = text
        .char_indices()
        .find(|(index, _)| {
            text[*index..]
                .get(..LEAD.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(LEAD))
        })?
        .0
        + LEAD.len();
    let stamp = text[start..]
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ';', ')']);
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// Codex's `usage limit for <model>`, lowercased like crew models compare.
fn limit_model(folded: &str) -> Option<String> {
    const LEAD: &str = "usage limit for ";
    let start = folded.find(LEAD)? + LEAD.len();
    let model = folded[start..].split_whitespace().next()?;
    let model = model.trim_end_matches(['.', ',', ';', ':', ')']);
    (!model.is_empty()).then(|| model.to_string())
}

/// `Resets in 1h37m37s`, `try again in 5 minutes`.
fn relative_reset(folded: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    ["resets in ", "reset in ", "try again in ", "retry in "]
        .iter()
        .find_map(|lead| {
            let start = folded.find(lead)? + lead.len();
            parse_duration(&folded[start..])
        })
        .and_then(|duration| now.checked_add_signed(duration))
}

/// A duration such as `1h37m37s`, `2 hours 5 minutes` or `45s`, read from
/// the start of `text`. `None` when it starts with no duration.
fn parse_duration(text: &str) -> Option<Duration> {
    let mut rest = text.trim_start();
    let mut total = Duration::zero();
    let mut parsed = false;
    loop {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        let Ok(value) = rest[..digits].parse::<i64>() else {
            return None;
        };
        let after = rest[digits..].trim_start();
        let unit_len = after.chars().take_while(char::is_ascii_alphabetic).count();
        let unit = match &after[..unit_len] {
            "d" | "day" | "days" => TimeDelta::try_days(value)?,
            "h" | "hr" | "hrs" | "hour" | "hours" => TimeDelta::try_hours(value)?,
            "m" | "min" | "mins" | "minute" | "minutes" => TimeDelta::try_minutes(value)?,
            "s" | "sec" | "secs" | "second" | "seconds" => TimeDelta::try_seconds(value)?,
            _ => break,
        };
        total = total.checked_add(&unit)?;
        parsed = true;
        rest = after[unit_len..].trim_start_matches([' ', ',']);
        rest = rest.strip_prefix("and ").unwrap_or(rest);
    }
    parsed.then_some(total)
}

/// Codex's `Try again at 3:42 PM.` or `Try again at Oct 9th, 2026 3:42 PM.`,
/// in the host's local time.
fn try_again_at(folded: &str, now: DateTime<Utc>, local: FixedOffset) -> Option<DateTime<Utc>> {
    const LEAD: &str = "try again at ";
    let start = folded.find(LEAD)? + LEAD.len();
    let rest = &folded[start..];
    let end = rest
        .find(". ")
        .or_else(|| rest.find(".\n"))
        .unwrap_or(rest.len());
    let stamp = rest[..end].trim().trim_end_matches('.').trim();
    let naive = match NaiveTime::parse_from_str(stamp, "%I:%M %p") {
        Ok(time) => now.with_timezone(&local).date_naive().and_time(time),
        Err(_) => {
            let unordinal = ["st,", "nd,", "rd,", "th,"]
                .iter()
                .fold(stamp.to_string(), |text, suffix| text.replace(suffix, ","));
            NaiveDateTime::parse_from_str(&unordinal, "%b %d, %Y %I:%M %p").ok()?
        }
    };
    local
        .from_local_datetime(&naive)
        .single()
        .map(|at| at.with_timezone(&Utc))
}

/// Claude Code's `resets 3pm (America/Los_Angeles)` or `resets Oct 10, 3pm
/// (Europe/London)`. A reset already past today is tomorrow's.
fn resets_at_zone(folded: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    const LEAD: &str = "resets ";
    let start = folded.find(LEAD)? + LEAD.len();
    let rest = &folded[start..];
    let open = rest.find('(')?;
    let close = open + rest[open..].find(')')?;
    let zone_text = rest[open + 1..close].trim();
    // Exclude the IANA placeholder and non-name bytes before the database's
    // case-insensitive lookup, which can equate DEL with an underscore.
    if zone_text == "factory"
        || !zone_text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'+' | b'-'))
    {
        return None;
    }
    let zone = *tzdb_data::find_tz(zone_text.as_bytes())?;
    let when = rest[..open].trim().trim_end_matches(',');
    let (date, time) = match when.rsplit_once(", ") {
        Some((date, time)) => (Some(date.trim()), time.trim()),
        None => (None, when),
    };
    let time = parse_clock(time)?;
    let observed_offset = zone.find_local_time_type(now.timestamp()).ok()?.ut_offset();
    let observed_offset = FixedOffset::east_opt(observed_offset)?;
    let today = now.naive_utc().checked_add_offset(observed_offset)?.date();
    let date = match date {
        Some(date) => nearest_explicit_date(date, today)?,
        None => today,
    };
    let at = resolve_named_reset_utc(zone, date, time)?;
    if at <= now && date == today {
        resolve_named_reset_utc(zone, date.succ_opt()?, time)
    } else {
        Some(at)
    }
}

fn resolve_named_reset_utc(
    zone: TimeZoneRef<'_>,
    date: NaiveDate,
    time: NaiveTime,
) -> Option<DateTime<Utc>> {
    let candidates = tz::DateTime::find(
        date.year(),
        date.month() as u8,
        date.day() as u8,
        time.hour() as u8,
        time.minute() as u8,
        0,
        0,
        zone,
    )
    .ok()?;
    // Candidates are in UTC order. Take the first real instant in a fold
    // and reject a clock time skipped by a forward transition.
    let at = candidates
        .into_inner()
        .into_iter()
        .find_map(|candidate| match candidate {
            FoundDateTimeKind::Normal(at) => Some(at),
            FoundDateTimeKind::Skipped { .. } => None,
        })?;
    DateTime::from_timestamp(at.unix_time(), 0)
}

/// The year-less `Jan 2` occurrence nearest `today`, so a January reset seen in
/// late December is next January's, and a December reset seen in early January
/// is last December's. A date already passed today is not pushed a year ahead:
/// its own occurrence is nearest, and the caller rolls a past instant to tomorrow.
fn nearest_explicit_date(date: &str, today: NaiveDate) -> Option<NaiveDate> {
    [today.year() - 1, today.year(), today.year() + 1]
        .into_iter()
        .filter_map(|year| NaiveDate::parse_from_str(&format!("{date} {year}"), "%b %d %Y").ok())
        .min_by_key(|candidate| (*candidate - today).num_days().abs())
}

/// `3pm`, `3:30pm`, `10 am`.
fn parse_clock(text: &str) -> Option<NaiveTime> {
    let compact = text.replace(' ', "");
    let (clock, meridiem) = compact.split_at_checked(compact.len().checked_sub(2)?)?;
    if meridiem != "am" && meridiem != "pm" {
        return None;
    }
    let (hour, minute) = match clock.split_once(':') {
        Some((hour, minute)) => (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?),
        None => (clock.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&hour) {
        return None;
    }
    let hour = hour % 12 + if meridiem == "pm" { 12 } else { 0 };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

/// Claude's older `Claude AI usage limit reached|<epoch seconds>`.
fn epoch_suffix(folded: &str) -> Option<DateTime<Utc>> {
    const LEAD: &str = "usage limit reached|";
    let start = folded.find(LEAD)? + LEAD.len();
    let digits: String = folded[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    DateTime::from_timestamp(digits.parse().ok()?, 0)
}
