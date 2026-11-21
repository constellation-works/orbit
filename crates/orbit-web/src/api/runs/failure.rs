//! Compatibility re-exports for the run-failure leaf.
//!
//! The implementation lives in [`crate::run_failure`]. Retirement: callers
//! import `crate::run_failure` directly, then this module goes away.

pub(super) use crate::run_failure::attach_run_failure_context;
