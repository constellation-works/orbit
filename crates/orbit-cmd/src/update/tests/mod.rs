//! Sibling unit tests for the `orbit update` pipeline.

mod fixture;
mod flow;
mod stage;

#[cfg(unix)]
mod converge;
