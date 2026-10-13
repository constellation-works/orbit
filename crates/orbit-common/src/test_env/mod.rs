//! Ambient-process isolation and validation of re-executed child tests.
//!
//! Environment variables and umask need isolation because a test that depends
//! on ambient process state passes or fails according to *how the suite was
//! launched* rather than what the code does. See [`unset`] for inherited env
//! vars and [`harden_dir`] for umask-derived directory permissions.
//!
//! Several Orbit surfaces read agent identity and run context from the
//! process environment — notably runtime actor identity and `tool run`
//! audit-role resolution. A test that asserts the *absence* of that context
//! ("attributes to the human actor", "falls back to the agent role") is only
//! correct when those variables are genuinely unset.
//!
//! GitHub CI runs the suite from a bare shell, so the assertions hold there by
//! accident. An agent running the same suite from inside a managed Orbit run
//! inherits `ORBIT_RUN_ID`, `ORBIT_AGENT_MODEL`, `ORBIT_TASK_ID`, … and those
//! tests flip to red for a reason that has nothing to do with the code under
//! test (ORB-10350). [`unset`] makes the expectation explicit instead of
//! ambient.
//!
//! Integration tests that *spawn* the `orbit` binary need the same defense
//! one process out, where the stakes include durable routing rather than just
//! attribution. [`INHERITED_AUTHORITY_ENV`] is the canonical list for that
//! case and [`clear_inherited_authority`] applies it.
//!
//! A whole test binary gets the same defense from [`isolate_test_process!`](crate::isolate_test_process):
//! it clears [`INHERITED_AUTHORITY_ENV`] before `main`, so a bare `cargo test`
//! from an agent shell behaves like one from a clean shell (ORB-14926).
//!
//! Always available so integration tests and sibling crates share one
//! implementation without changing `orbit-common`'s feature set. Child-test
//! guards reject successful libtest exits that never executed the exact filter.

mod child;
mod env;
#[cfg(unix)]
mod fifo;
mod orphan_watchdog;
mod process;
mod process_scrub;
mod temp_dir;

pub use child::{
    CHILD_TEST_CEILING, CHILD_TEST_DEADLINE, FIXTURE_CEILING, FIXTURE_STALL_DEADLINE,
    FIXTURE_STEP_DEADLINE, FixtureProgress, assert_child_test_exists, assert_child_test_passed,
    assert_step_finished, run_child_test, run_child_test_within,
};
pub(crate) use env::scoped_env_active;
pub use env::{
    DurableSyncs, INHERITED_AUTHORITY_ENV, MANAGED_RUN_ENV, ScopedEnv, clear_inherited_authority,
    require_durable_syncs, scoped, unset,
};
#[cfg(unix)]
pub use fifo::{create_fixture_fifo, release_fixture_fifo};
pub(crate) use process::classify_ps;
pub use process::{
    PsRun, harden_dir, host_load, prime_executable, ps_denied_deferral_notice, ps_lstart_utc,
    ps_stat, report_ps_denied, start_identity_probe_blocker, wait_until,
};
#[cfg(unix)]
pub use process::{UnrelatedProcess, spawn_unrelated_process};
pub use process_scrub::{SCRUBBED_MARKER_ENV, scrub_inherited_authority};
pub use temp_dir::{bulk_write_temp_dir, canonical_temp_dir};

/// Clock seam for tests of callers that use fixed supervision deadlines.
#[cfg(feature = "test-support")]
pub use crate::process::supervisor::seam::with_supervision_clock;

/// Supervision seams for tests of a supervisor's callers in other crates.
pub use crate::process::supervisor::seam::{SupervisorSeam, WaitHook, with_supervisor_seam};
