//! Deterministic child environments for agent subprocesses.
//!
//! An agent subprocess is untrusted and keeps host network access, so its
//! environment is *composed from an allowlist* rather than filtered out of the
//! parent process. The distinction is the whole point of this module: a
//! denylist admits every name nobody thought to forbid, so a benignly named
//! credential (`DATABASE_URL`, an internal service URL, a per-team
//! `BILLING_ENDPOINT`) reaches the provider CLI even though the operator
//! configured `inherit = false`.
//!
//! Admission control here is therefore membership in an explicit set of names:
//! the documented baseline below, the operator's configured pass list, the
//! extras a provider declares it requires, and the named `ORBIT_*` execution
//! envelope below. The `ORBIT_` prefix is *not* a wildcard: privilege-bearing
//! names in that namespace (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`,
//! `ORBIT_MCP_SSH_ACCEPTANCE`) must not reach an untrusted child.
//! Credential-name and value-shape heuristics are deliberately *not*
//! consulted — they cannot classify names an operator's environment actually
//! uses, and treating them as a gate is what let the bypass exist.
//!
//! [`inherited_child_env`] is the explicit opt-out for an operator who really
//! does want full inheritance.

use std::collections::{BTreeMap, BTreeSet};

/// Runtime context every supported provider CLI needs in order to start.
///
/// Kept explicit so the effective floor of a child environment is reviewable
/// instead of being whatever the dispatching process happened to inherit.
/// Every entry is non-credential process context — login identity, filesystem
/// locations, locale, terminal shape. Provider credentials and service
/// endpoints are never baseline; an operator opts into those by name through
/// `[execution.env].pass`.
pub const AGENT_SUBPROCESS_BASELINE_VARS: &[&str] = &[
    "HOME", "LANG", "LC_ALL", "LOGNAME", "PATH", "SHELL", "TERM", "TMPDIR", "TZ", "USER",
];

/// Exact envelope names a managed run exports or forwards into a child.
///
/// The provenance subset (`ORBIT_RUN_ID`, `ORBIT_MANAGED_RUN_CONTEXT`,
/// `ORBIT_AGENT_NAME`, `ORBIT_AGENT_MODEL`, `ORBIT_SESSION_ID`, `ORBIT_TASK_ID`,
/// `ORBIT_ACTIVE_TASK_ID`) is built in `orbit-engine` `context/env.rs`. The
/// remaining names are locators and activity bindings the CLI runner injects
/// (and that a parent process may already hold). Privilege-bearing names in the
/// same `ORBIT_` namespace are absent from this list on purpose.
const ORBIT_ENVELOPE_VARS: &[&str] = &[
    "ORBIT_RUN_ID",
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_SESSION_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_ROOT",
    "ORBIT_REGISTRY_ROOT",
    "ORBIT_WORKSPACE",
    "ORBIT_WORKTREE_ROOT",
    "ORBIT_BIN",
    "ORBIT_STEP_INDEX",
    "ORBIT_TASK_ACTOR_KIND",
];

/// Envelope families admitted by prefix because the engine treats them as
/// groups (`ORBIT_ACTIVITY_ID` / `_TOOLS` / `_FS_PROFILE`, and the search
/// companion override cluster).
const ORBIT_ENVELOPE_PREFIXES: &[&str] = &["ORBIT_ACTIVITY_", "ORBIT_SEARCH_COMPANION"];

fn is_orbit_envelope_name(name: &str) -> bool {
    ORBIT_ENVELOPE_VARS.contains(&name)
        || ORBIT_ENVELOPE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// The environment an allowlist-governed agent subprocess is launched with:
/// [`AGENT_SUBPROCESS_BASELINE_VARS`], the configured `pass` names, the
/// `extras` a provider declares it requires, and the named Orbit execution
/// envelope — each included only when the parent process actually holds it.
///
/// Names outside that set are absent from the result, so the caller can launch
/// the child from a cleared environment and get exactly this.
pub fn allowlisted_child_env(pass: &[String], extras: &[&str]) -> Vec<(String, String)> {
    allowlisted_child_env_from(&std::env::vars().collect::<Vec<_>>(), pass, extras)
}

/// Snapshot-driven form of [`allowlisted_child_env`].
///
/// Taking the parent environment as data is what makes the admission rule
/// testable: a test states the ambient environment it is reasoning about
/// instead of inheriting the developer's shell.
pub fn allowlisted_child_env_from(
    parent: &[(String, String)],
    pass: &[String],
    extras: &[&str],
) -> Vec<(String, String)> {
    let admitted: BTreeSet<&str> = AGENT_SUBPROCESS_BASELINE_VARS
        .iter()
        .copied()
        .chain(pass.iter().map(String::as_str))
        .chain(extras.iter().copied())
        .collect();
    let mut env: BTreeMap<String, String> = parent
        .iter()
        .filter(|(name, _)| admitted.contains(name.as_str()) || is_orbit_envelope_name(name))
        .cloned()
        .collect();
    backfill_login_identity(&mut env);
    env.into_iter().collect()
}

/// Full inheritance of the parent environment.
///
/// The explicit opt-in behind `[execution.env] inherit = true`: every ambient
/// variable, credentials included, reaches the child. Callers that do not opt
/// in use [`allowlisted_child_env`].
pub fn inherited_child_env() -> Vec<(String, String)> {
    std::env::vars().collect()
}

/// Ensure a child environment carries a login identity (`USER` / `LOGNAME`).
///
/// A provider CLI such as `claude` reads `USER` to locate its per-user
/// credential store (macOS Keychain account). When Orbit's executor is started
/// without a login environment — e.g. a detached pipeline worker that did not
/// inherit a login shell's variables — those names are absent or empty and the
/// spawned provider fails to authenticate (HTTP 401) even though valid
/// credentials exist. Backfill the real OS login name resolved from the current
/// uid so the child always has a correct identity; an already-present, non-empty
/// value is never overwritten. [ORB-00409]
// pub(crate) for sibling-layout tests in security/tests/child_env.rs.
pub(crate) fn backfill_login_identity(vars: &mut BTreeMap<String, String>) {
    let missing = |vars: &BTreeMap<String, String>, key: &str| {
        vars.get(key).is_none_or(|value| value.is_empty())
    };
    let need_user = missing(vars, "USER");
    let need_logname = missing(vars, "LOGNAME");
    if !need_user && !need_logname {
        return;
    }
    let Some(login) = os_login_name() else {
        // Cannot resolve a login name; leave the environment untouched rather
        // than inject a placeholder that would itself fail credential lookup.
        return;
    };
    if need_user {
        vars.insert("USER".to_string(), login.clone());
    }
    if need_logname {
        vars.insert("LOGNAME".to_string(), login);
    }
}

/// Resolve the current process's OS login name.
///
/// On Unix this is the `pw_name` for the real uid via the reentrant
/// `getpwuid_r`, which (unlike `$USER`) does not depend on the ambient
/// environment. Returns `None` when no passwd entry exists or the lookup
/// fails.
// pub(crate) for sibling-layout tests in security/tests/child_env.rs.
#[cfg(unix)]
pub(crate) fn os_login_name() -> Option<String> {
    use std::ffi::CStr;

    // SAFETY: getuid cannot fail. getpwuid_r writes into caller-owned buffers
    // (`pwd` and `buf`); we only read `pw_name` after a success (rc == 0) with a
    // non-null `result`, and copy it out before either buffer is dropped.
    let uid = unsafe { libc::getuid() };
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == 0 {
            if result.is_null() || pwd.pw_name.is_null() {
                return None;
            }
            let name = unsafe { CStr::from_ptr(pwd.pw_name) };
            return name
                .to_str()
                .ok()
                .map(str::to_owned)
                .filter(|name| !name.is_empty());
        }
        // ERANGE: buffer too small. Grow and retry up to a sane ceiling.
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return None;
    }
}

/// Non-Unix fallback: derive the login name from `USERNAME` if present.
// pub(crate) for sibling-layout tests in security/tests/child_env.rs.
#[cfg(not(unix))]
pub(crate) fn os_login_name() -> Option<String> {
    std::env::var("USERNAME")
        .ok()
        .filter(|name| !name.is_empty())
}
