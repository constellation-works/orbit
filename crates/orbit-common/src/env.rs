//! The one capture of the process environment.
//!
//! Each binary's `main` calls [`Env::capture`] once, before logging starts,
//! and passes the value to composition. [`env_vars`](crate::env_vars)
//! declares every fixed name and gives each a typed accessor on [`Env`];
//! [`Env::var`] and [`Env::vars`] serve a name known only at run time and a
//! child process's environment.

#![expect(
    clippy::disallowed_methods,
    reason = "This module owns the process snapshot and scoped test environment mutations"
)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::sync::Arc;

/// Prefix of the test hooks, which only tests and `test-support` builds read.
const TEST_HOOK_PREFIX: &str = "ORBIT_TEST_";

/// An immutable snapshot of the process environment.
///
/// Cloning shares the snapshot. Nothing changes it after construction, so a
/// variable set after capture is not seen.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: Arc<BTreeMap<OsString, OsString>>,
}

impl Env {
    /// Capture the process environment. Call this once, in `main`.
    #[must_use]
    pub fn capture() -> Self {
        Self::from_vars(std::env::vars_os())
    }

    /// Build an environment from a given variable list without reading the
    /// process environment. A repeated name keeps its last value.
    #[must_use]
    pub fn from_vars<I, K, V>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self {
            vars: Arc::new(
                vars.into_iter()
                    .map(|(name, value)| (name.into(), value.into()))
                    .collect(),
            ),
        }
    }

    /// The value of a variable whose name is known only at run time, such as
    /// the secret a plugin manifest names. Fixed names use their accessor.
    /// A test hook is unset here outside tests and `test-support` builds.
    pub fn var(&self, name: &str) -> Option<&OsStr> {
        if !cfg!(any(test, feature = "test-support"))
            && name.get(..TEST_HOOK_PREFIX.len()).is_some_and(|prefix| {
                prefix == TEST_HOOK_PREFIX
                    || cfg!(windows) && prefix.eq_ignore_ascii_case(TEST_HOOK_PREFIX)
            })
        {
            return None;
        }
        self.get(name)
    }

    /// Every captured variable, ordered by name, for building a child
    /// process's environment.
    pub fn vars(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str()))
    }

    /// The captured environment in the UTF-8 representation used by subprocess
    /// policies and configuration. Non-Unicode entries cannot be represented.
    pub fn utf8_vars(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars()
            .filter_map(|(name, value)| Some((name.to_str()?, value.to_str()?)))
    }

    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        let value = self.vars.get(OsStr::new(name));
        // Windows looks up environment names without regard to case. Keep
        // captured spelling for child inheritance, including the usual Path.
        #[cfg(windows)]
        let value = value.or_else(|| {
            self.vars.iter().find_map(|(key, value)| {
                key.to_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(name))
                    .then_some(value)
            })
        });
        value.map(OsString::as_os_str)
    }
}

/// Lists names only: captured values can hold credentials.
impl fmt::Debug for Env {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Env")
            .field("names", &self.vars.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Set a process environment variable for the scoped test environment helpers.
///
/// # Safety
/// The caller must prevent concurrent access to the process environment.
pub(crate) unsafe fn set_process_var(name: &str, value: &str) {
    // SAFETY: the caller upholds the process-environment synchronization contract.
    unsafe { std::env::set_var(name, value) }
}

/// Remove a process environment variable for the test-process startup scrub.
///
/// # Safety
/// The caller must prevent concurrent access to the process environment.
pub(crate) unsafe fn remove_process_var(name: &str) {
    // SAFETY: the caller upholds the process-environment synchronization contract.
    unsafe { std::env::remove_var(name) }
}
