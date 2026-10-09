//! Pre-`main` scrub of inherited managed-run authority from a test process.

use super::INHERITED_AUTHORITY_ENV;

/// Set by the scrub so a test process re-executed by a scrubbed parent keeps
/// the variables its parent set on purpose.
pub const SCRUBBED_MARKER_ENV: &str = "ORBIT_TEST_ENV_SCRUBBED";

/// Remove every [`INHERITED_AUTHORITY_ENV`] variable from this process, once.
///
/// Called by [`isolate_test_process!`](crate::isolate_test_process) before
/// `main`, while the process is single-threaded. A child that a scrubbed test
/// re-executes inherits [`SCRUBBED_MARKER_ENV`] and is left alone, so a
/// variable the parent exported for the child on purpose reaches its body.
pub fn scrub_inherited_authority() {
    if std::env::var_os(SCRUBBED_MARKER_ENV).is_some() {
        return;
    }
    // SAFETY: runs from a pre-`main` initializer, before any thread exists.
    unsafe {
        for name in INHERITED_AUTHORITY_ENV {
            std::env::remove_var(name);
        }
        std::env::set_var(SCRUBBED_MARKER_ENV, "1");
    }
}

/// Run [`scrub_inherited_authority`] before `main` in the current test binary.
///
/// A test process launched from a managed run inherits its `ORBIT_*`
/// authority, and `ORBIT_WORKER_CONTEXT_REQUIRED` makes every in-process
/// runtime fixture fail closed with "managed worker runtime binding
/// unavailable". `make` gates strip it, a bare `cargo test` did not
/// (ORB-14926). Invoke once at the root of each crate under `#[cfg(test)]`,
/// or at the top of an integration-test file. Never from production code: it
/// would clear a real worker's authority.
///
/// Supported on Linux, macOS and Windows; elsewhere the invocation is empty.
#[macro_export]
macro_rules! isolate_test_process {
    () => {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        const _: () = {
            extern "C" fn scrub() {
                $crate::test_env::scrub_inherited_authority();
            }
            #[used]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
            #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
            #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
            static SCRUB_AT_LOAD: extern "C" fn() = scrub;
        };
    };
}
