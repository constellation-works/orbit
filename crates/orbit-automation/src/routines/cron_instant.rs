//! croner's search boundary over Orbit's chrono timestamps [ORB-25521].
//!
//! croner is built without its `chrono` backend feature, so it carries no time
//! library of its own. [`CronInstant`] lends croner a chrono `DateTime<Tz>`:
//! the wall clock and weekday come from `Tz`'s local view, and a wall clock is
//! resolved back through `Tz` itself, so `Local` keeps reading the host zone
//! exactly as before. Every caller of the due math still passes and receives
//! chrono values; this wrapper never leaves [`super::due`].
//!
//! chrono's Unix `Local` answers a repeated wall clock with the two readings
//! ordered by offset, which puts the later instant first, and can offer a
//! reading at a transition edge that the zone never shows, such as
//! 03:00 +02:00 at Berlin's autumn change. croner needs the real readings,
//! earliest first, so [`resolve_civil`](CronDateTime::resolve_civil) keeps only
//! a reading whose offset the zone itself gives that instant, and orders them.

use std::cmp::Ordering;

use chrono::{
    DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeDelta, TimeZone,
    Timelike,
};
use croner::errors::CronError;
use croner::{CivilDate, CivilDateTime, CivilTime, CronDateTime, Resolution, Weekday};

/// A chrono instant as croner searches it.
#[derive(Clone)]
pub(super) struct CronInstant<Tz: TimeZone>(pub(super) DateTime<Tz>);

impl<Tz: TimeZone> CronDateTime for CronInstant<Tz> {
    fn to_civil(&self) -> CivilDateTime {
        let naive = self.0.naive_local();
        // chrono keeps every part in range and holds a leap second in the
        // nanosecond part, so the parts are valid as they are.
        CivilDateTime::new(
            CivilDate::from_parts_unchecked(naive.year(), naive.month(), naive.day()),
            CivilTime::from_parts_unchecked(naive.hour(), naive.minute(), naive.second()),
        )
    }

    fn civil_weekday(&self) -> Weekday {
        Weekday::from_days_from_sunday(self.0.naive_local().weekday().num_days_from_sunday())
    }

    fn resolve_civil(&self, civil: CivilDateTime) -> Result<Resolution<Self>, CronError> {
        let naive = civil_to_naive(civil)?;
        let zone = self.0.timezone();
        let (first, second) = match zone.from_local_datetime(&naive) {
            LocalResult::Single(reading) => (Some(reading), None),
            LocalResult::Ambiguous(one, other) => (Some(one), Some(other)),
            LocalResult::None => (None, None),
        };
        let real = |reading: &DateTime<Tz>| {
            zone.offset_from_utc_datetime(&reading.naive_utc()).fix() == reading.offset().fix()
        };
        Ok(match (first.filter(&real), second.filter(&real)) {
            (Some(one), Some(other)) if one == other => Resolution::Single(Self(one)),
            (Some(one), Some(other)) => {
                let (earlier, later) = if one < other {
                    (one, other)
                } else {
                    (other, one)
                };
                Resolution::Ambiguous(Self(earlier), Self(later))
            }
            (Some(only), None) | (None, Some(only)) => Resolution::Single(Self(only)),
            (None, None) => Resolution::Gap,
        })
    }

    fn checked_add_seconds(&self, seconds: i64) -> Option<Self> {
        self.0
            .clone()
            .checked_add_signed(TimeDelta::try_seconds(seconds)?)
            .map(Self)
    }

    fn cmp_instant(&self, other: &Self) -> Ordering {
        // chrono orders a `DateTime` by its instant, not its wall clock, so
        // the two halves of a repeated local hour compare unequal.
        self.0.cmp(&other.0)
    }
}

fn civil_to_naive(civil: CivilDateTime) -> Result<NaiveDateTime, CronError> {
    NaiveDate::from_ymd_opt(civil.year(), civil.month(), civil.day())
        .ok_or(CronError::InvalidDate)?
        .and_hms_opt(civil.hour(), civil.minute(), civil.second())
        .ok_or(CronError::InvalidTime)
}
