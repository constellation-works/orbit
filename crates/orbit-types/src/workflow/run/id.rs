//! Job run identifier composition and the role a run id declares.
//!
//! A minted id is `jrun-<YYYYmmdd-HHMM>-<role><sequence>`. Runs pile into one
//! minute stem from two unrelated directions — a second direct submission and
//! the first run's own child dispatch both land there — so a bare sequence
//! number left a top-level run and an unrelated run's child reading
//! identically [ORB-12111]. The role letter is what makes the id alone answer
//! which one it is.
//!
//! Ids minted before the role letter existed carry a bare stem or a plain
//! numeric sequence. They stay readable, and [`run_id_role`] reports `None`
//! for them rather than claiming a role their suffix never encoded.

use std::fmt;

use chrono::{DateTime, Utc};

/// Prefix shared by every job run id.
const RUN_ID_PREFIX: &str = "jrun-";

/// Whether a run was submitted on its own or dispatched by a parent run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunIdRole {
    /// Submitted directly: a CLI submission, an automation key, or a resume.
    TopLevel,
    /// Dispatched by a running parent's activity.
    Child,
}

impl RunIdRole {
    /// The letter this role writes into a minted id. Each role numbers its own
    /// sequence, so `-t2` and `-c2` can share a minute without colliding.
    const fn marker(self) -> char {
        match self {
            Self::TopLevel => 't',
            Self::Child => 'c',
        }
    }

    /// The inverse of [`Self::marker`], kept beside it so the two stay in step.
    const fn from_marker(marker: char) -> Option<Self> {
        match marker {
            't' => Some(Self::TopLevel),
            'c' => Some(Self::Child),
            _ => None,
        }
    }
}

impl fmt::Display for RunIdRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::TopLevel => "top-level",
            Self::Child => "child",
        };
        formatter.write_str(label)
    }
}

/// The stem every run submitted in the same minute shares.
pub fn run_id_minute_stem(submitted_at: DateTime<Utc>) -> String {
    format!("{RUN_ID_PREFIX}{}", submitted_at.format("%Y%m%d-%H%M"))
}

/// The `sequence`-th id of `role` within `minute_stem`, counting from 1.
pub fn run_id_candidate(minute_stem: &str, role: RunIdRole, sequence: u32) -> String {
    format!("{minute_stem}-{}{sequence}", role.marker())
}

/// The role `run_id` declares, or `None` when its suffix encodes no role.
pub fn run_id_role(run_id: &str) -> Option<RunIdRole> {
    if !run_id.starts_with(RUN_ID_PREFIX) {
        return None;
    }

    let suffix = run_id.rsplit_once('-')?.1;
    let mut characters = suffix.chars();
    let role = RunIdRole::from_marker(characters.next()?)?;

    // A marker without a sequence is some other suffix that happens to start
    // with the same letter, not a role.
    let sequence = characters.as_str();
    let numbered = !sequence.is_empty() && sequence.bytes().all(|byte| byte.is_ascii_digit());
    numbered.then_some(role)
}
