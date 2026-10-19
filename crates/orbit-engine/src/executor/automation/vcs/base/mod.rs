//! Keeping a delivery current with its base: claimed known-red replays,
//! advanced-base chases, obsolete-base refusals and landing base refreshes.

pub(super) mod chase;
pub(super) mod claim;
pub(super) mod obsolescence;
pub(super) mod refresh;

#[cfg(test)]
mod tests;
