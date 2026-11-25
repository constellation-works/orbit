//! The one capture of the process environment.
//!
//! Each binary's `main` calls [`Env::capture`] once, before logging starts,
//! and passes the value to composition. [`env_vars`](crate::env_vars)
//! declares every fixed name and gives each a typed accessor on [`Env`];
//! [`Env::var`] and [`Env::vars`] serve a name known only at run time and a
//! child process's environment.

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
#[derive(Clone, Default)]
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
        if !cfg!(any(test, feature = "test-support")) && name.starts_with(TEST_HOOK_PREFIX) {
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

    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
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

#[cfg(test)]
mod tests {
    use super::Env;
    use crate::env_vars;
    use std::ffi::OsStr;

    #[test]
    fn from_vars_serves_accessors_and_lists_without_the_process_environment() {
        let env = Env::from_vars([
            (env_vars::HOME, "/home/orbit"),
            ("PLUGIN_SECRET", "first"),
            ("PLUGIN_SECRET", "second"),
        ]);

        assert_eq!(env.home(), Some(OsStr::new("/home/orbit")));
        assert_eq!(env.cargo_home(), None);
        assert_eq!(env.var("PLUGIN_SECRET"), Some(OsStr::new("second")));
        assert_eq!(
            env.vars().collect::<Vec<_>>(),
            [
                (OsStr::new("HOME"), OsStr::new("/home/orbit")),
                (OsStr::new("PLUGIN_SECRET"), OsStr::new("second")),
            ]
        );
        assert!(!format!("{env:?}").contains("second"));
    }
}
