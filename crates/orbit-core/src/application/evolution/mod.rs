//! Workspace capability-gap ledger and proposal suppression.
mod operations;

/// Days of evolution-ledger cost the resident digest and a decline baseline
/// count.
///
/// The former `resident.evolution.window_days` default, fixed when the key was removed [ORB-22808].
pub(crate) const EVOLUTION_WINDOW_DAYS: u32 = 14;

/// Days a rejected evolution proposal stays declined when the decline names
/// no days of its own.
///
/// The former `resident.evolution.decline_suppress_days` default, fixed when the key was removed [ORB-22808].
pub(crate) const EVOLUTION_DECLINE_SUPPRESS_DAYS: u32 = 30;
