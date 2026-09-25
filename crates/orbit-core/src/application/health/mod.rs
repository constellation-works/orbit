//! Standing health of the definition artifacts a workspace accumulates, and
//! the activity-catalog checks and repairs `orbit doctor` runs on them.

pub(crate) mod activity_catalog;
pub mod artifact;
mod artifact_diagnosis;

#[cfg(test)]
mod tests;
