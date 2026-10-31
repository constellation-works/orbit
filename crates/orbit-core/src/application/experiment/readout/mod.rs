//! An experiment's readout, computed on read from its assignment rows joined
//! to the outcome records Orbit already keeps. The unit is the task and the
//! analysis is intention-to-treat: every enrolled, unforced task counts in
//! its arm whatever its outcome; forced assignments are left out and
//! counted.

mod build;
mod metrics;
mod model;
mod outcomes;
mod stats;
mod verdict;

pub use model::ExperimentReadout;
