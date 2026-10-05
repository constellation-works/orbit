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

/// Minimal in-memory run fixture: these tests do not open or mutate a store.
#[cfg(unix)]
fn owner_run(pid: Option<u32>, token: Option<String>) -> orbit_types::workflow::JobRun {
    serde_json::from_value(serde_json::json!({
        "run_id": "jrun-owner-signal-fixture",
        "job_id": "fixture",
        "attempt": 1,
        "state": "running",
        "scheduled_at": "2026-09-01T00:00:00Z",
        "created_at": "2026-09-01T00:00:00Z",
        "pid": pid,
        "pid_start_time": token,
    }))
    .unwrap()
}

/// Safety branch unreachable through public cancellation, which only invokes
/// signalling when a PID is present. No PID must never reach kill(0).
#[cfg(unix)]
#[test]
fn signal_owner_without_pid_returns_no_pid() {
    assert_eq!(
        super::super::owner::signal_run_owner_process(&owner_run(None, None)).unwrap(),
        "no_pid"
    );
}

/// A synthetic namespace mismatch exercises the safety guard without needing
/// namespace creation privileges or risking a real foreign process.
// Linux: /proc PID-namespace identity must refuse signalling a PID owned by another namespace.
#[cfg(target_os = "linux")]
#[test]
fn signal_owner_in_foreign_pid_namespace_never_signals_the_local_process() {
    let owner = orbit_common::test_env::spawn_unrelated_process();
    let token = format!("{STABLE_TOKEN_PREFIX}pidns=foreign:Sun Aug  2 20:13:45 2026");
    assert_eq!(
        super::super::owner::signal_run_owner_process(&owner_run(Some(owner.pid()), Some(token)))
            .unwrap(),
        "foreign_pid_namespace"
    );
    assert!(orbit_common::process::identity::process_is_alive(
        owner.pid()
    ));
}

/// Exercise a real reaped owner, rather than a mocked identity probe.
#[cfg(unix)]
#[test]
fn signal_owner_after_exit_returns_already_exited() {
    let owner = orbit_common::test_env::spawn_unrelated_process();
    let pid = owner.pid();
    drop(owner);
    assert_eq!(
        super::super::owner::signal_run_owner_process(&owner_run(Some(pid), None)).unwrap(),
        "already_exited"
    );
}
