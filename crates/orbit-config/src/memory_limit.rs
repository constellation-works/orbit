//! The one grammar for worker memory limits (`machine.worker_memory_*`)
//! [ORB-12913].
//!
//! Config admission parses the operator's string into a [`MemoryLimit`] built
//! only from integers and fixed tokens; consumers format it with `Display`
//! and never re-parse it, so there is no second grammar to drift and no
//! consumer-side parse failure that could quietly drop a limit.

use std::fmt;

use serde::{Serialize, Serializer};

/// A systemd memory limit (`MemoryHigh=` / `MemoryMax=`).
///
/// `Display` renders the systemd form: `<amount>[K|M|G|T]`, `<n>%` of
/// physical RAM, or `infinity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimit {
    /// A positive amount, in bytes or in the given binary unit.
    Bytes {
        /// Positive count of `unit`s (of bytes when `unit` is `None`).
        amount: u64,
        /// The suffix; `None` for a bare byte count.
        unit: Option<MemoryUnit>,
    },
    /// A percentage (1..=100) of physical RAM, resolved by systemd.
    Percent(u8),
    /// No limit.
    Infinity,
}

/// The size suffixes systemd accepts on a memory limit (powers of 1024).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryUnit {
    /// Kibibytes (`K`).
    K,
    /// Mebibytes (`M`).
    M,
    /// Gibibytes (`G`).
    G,
    /// Tebibytes (`T`).
    T,
}

impl MemoryUnit {
    const ALL: [Self; 4] = [Self::K, Self::M, Self::G, Self::T];

    fn suffix(self) -> char {
        match self {
            Self::K => 'K',
            Self::M => 'M',
            Self::G => 'G',
            Self::T => 'T',
        }
    }
}

impl MemoryLimit {
    /// Parse `infinity`, `<n>%` (1..=100), or a positive integer with an
    /// optional `K`/`M`/`G`/`T` suffix, ignoring surrounding whitespace.
    /// `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value == "infinity" {
            return Some(Self::Infinity);
        }
        if let Some(percent) = value.strip_suffix('%') {
            let percent = percent.parse::<u8>().ok()?;
            return (1..=100)
                .contains(&percent)
                .then_some(Self::Percent(percent));
        }
        let (digits, unit) = MemoryUnit::ALL
            .into_iter()
            .find_map(|unit| {
                value
                    .strip_suffix(unit.suffix())
                    .map(|digits| (digits, Some(unit)))
            })
            .unwrap_or((value, None));
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let amount = digits.parse::<u64>().ok()?;
        (amount > 0).then_some(Self::Bytes { amount, unit })
    }
}

impl fmt::Display for MemoryLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes { amount, unit } => {
                write!(formatter, "{amount}")?;
                match unit {
                    Some(unit) => write!(formatter, "{}", unit.suffix()),
                    None => Ok(()),
                }
            }
            Self::Percent(percent) => write!(formatter, "{percent}%"),
            Self::Infinity => formatter.write_str("infinity"),
        }
    }
}

/// `orbit config get`/`show` project the limit as its systemd string.
impl Serialize for MemoryLimit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
