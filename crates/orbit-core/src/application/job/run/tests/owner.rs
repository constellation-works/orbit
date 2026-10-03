//! Owner identity classification across PID namespaces [ORB-10594].

#[cfg(unix)]
use super::super::owner::{OwnerIdentity, classify_run_owner_with_probes};
#[cfg(unix)]
use orbit_common::process::identity::STABLE_TOKEN_PREFIX;
#[cfg(unix)]
use orbit_common::process::identity::{PidNamespaceScope, ProbeOutcome};

// ---- Cross-PID-namespace regression coverage (ORB-10594) ----
//
// Incident 2026-08-02: `jrun-20260802-2013-2` ran to a complete success (PR
// opened at 20:55:49Z) but its run record said `interrupted` since 20:43:00Z,
// because an Orbit CLI invoked by a sandboxed agent — `bwrap --unshare-all
// --proc /proc`, i.e. a private PID namespace — swept for orphans. From inside
// that namespace the host worker PIDs are invisible, so `ps` and
// `kill(pid, 0)` both reported "gone" for three healthy runs at once.

#[cfg(unix)]
#[test]
fn foreign_pid_namespace_never_condemns_a_live_owner() {
    // The exact incident shape: the observer is in another PID namespace, so
    // every probe available to it says the owner is gone — `ps` finds no
    // process and `kill(pid, 0)` agrees. Pre-fix this was `Missing`, which is
    // the stale set. The recorded owner was in fact mid-run.
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns=4026531836:Sun Aug  2 20:13:45 2026");
    let identity = classify_run_owner_with_probes(
        Some(83327),
        Some(persisted.as_str()),
        PidNamespaceScope::Foreign,
        |_| ProbeOutcome::NoProcess,
        |_| false,
        |_| false,
    );
    assert_eq!(identity, OwnerIdentity::ForeignPidNamespace);
}

#[cfg(unix)]
#[test]
fn same_pid_namespace_still_detects_a_genuinely_dead_owner() {
    // The distinguishing case: identical probe answers, but the observer
    // shares the owner's namespace, so "not found" really does mean dead.
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns=4026531836:Sun Aug  2 20:13:45 2026");
    let identity = classify_run_owner_with_probes(
        Some(83327),
        Some(persisted.as_str()),
        PidNamespaceScope::Same,
        |_| ProbeOutcome::NoProcess,
        |_| false,
        |_| false,
    );
    assert_eq!(identity, OwnerIdentity::Missing);
}
