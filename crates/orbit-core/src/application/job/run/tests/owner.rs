//! Owner and provider signalling safety with injected identity probes.

#[cfg(unix)]
use super::super::owner::{
    OwnerIdentity, ProviderStop, classify_run_owner_with_probes, stop_provider_process,
    stop_provider_process_with_probe,
};
#[cfg(unix)]
use orbit_common::process::identity::STABLE_TOKEN_PREFIX;
#[cfg(unix)]
use orbit_common::process::identity::{
    PidNamespaceScope, ProbeOutcome, ProcessLiveness, probe_process_start_identity,
    process_is_alive,
};

/// Fault injection: conservative Alive cannot authorize signalling when the
/// live start identity is unreadable, absent, or different [ORB-15618]. The
/// public cancellation boundary cannot force these kernel probe outcomes.
#[cfg(unix)]
#[test]
fn provider_signalling_requires_a_verified_live_identity() {
    let persisted = format!("{STABLE_TOKEN_PREFIX}pidns=-:Sun Aug  2 20:13:45 2026");
    for (probe, expected) in [
        (ProbeOutcome::Unavailable, ProviderStop::Unverified),
        (ProbeOutcome::NoProcess, ProviderStop::Unverified),
        (
            ProbeOutcome::Token(format!(
                "{STABLE_TOKEN_PREFIX}pidns=-:Sun Aug  2 20:13:46 2026"
            )),
            ProviderStop::NotRunning,
        ),
    ] {
        let provider = orbit_common::test_env::spawn_unrelated_process();
        let outcome = stop_provider_process_with_probe(
            provider.pid(),
            Some(&persisted),
            |_, _| ProcessLiveness::Alive,
            |_| probe.clone(),
        )
        .unwrap();
        assert_eq!(outcome, expected, "identity probe {probe:?}");
        assert!(
            process_is_alive(provider.pid()),
            "an unverified provider must receive no signal [ORB-15618]: {probe:?}"
        );
    }

    // Positive control: real probes and process-group teardown still stop an
    // owned child once its recorded start identity is positively verified.
    let provider = orbit_common::test_env::spawn_unrelated_process();
    let ProbeOutcome::Token(token) = probe_process_start_identity(provider.pid()) else {
        panic!("an owned provider child's start identity must be readable");
    };
    assert_eq!(
        stop_provider_process(provider.pid(), Some(&token)).unwrap(),
        ProviderStop::Stopped
    );
    assert!(
        !process_is_alive(provider.pid()),
        "verified provider teardown must leave the child stopped"
    );
}

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

/// Deterministic exit/reuse at cancellation boundaries that the runtime API
/// cannot force. Both callers share the same identity-bound teardown protocol.
#[cfg(unix)]
#[test]
fn owner_and_provider_never_escalate_against_a_replacement_identity() {
    use super::super::owner::{
        TeardownTarget, signal_run_owner_with_binding, stop_provider_process_with_binding,
    };
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    #[derive(Default)]
    struct State {
        generation: u32,
        signals: Vec<(u32, i32)>,
        waits: usize,
    }
    struct Target {
        state: Rc<RefCell<State>>,
        change_at: usize,
        grouped: bool,
        ignore_term: bool,
    }
    impl TeardownTarget for Target {
        fn pgid(&self) -> Option<libc::pid_t> {
            self.grouped.then_some(43210)
        }
        fn signal(&self, signal: i32) -> Result<(), orbit_common::OrbitError> {
            let mut state = self.state.borrow_mut();
            // Exit/reuse immediately before the first signal's delivery.
            if (self.change_at == 1 && state.signals.is_empty())
                || (self.change_at == 5 && signal == libc::SIGKILL)
            {
                state.generation = 1;
            }
            // A bound handle still addresses generation 0; no numeric fallback.
            if state.generation == 0 {
                state.signals.push((0, signal));
                if signal == libc::SIGKILL || !self.ignore_term {
                    state.generation = 1;
                }
            }
            Ok(())
        }
        fn liveness(&self) -> Result<(bool, bool), orbit_common::OrbitError> {
            Ok((
                self.state.borrow().generation == 0,
                self.grouped && self.change_at == 4,
            ))
        }
        fn wait(&self, _: Duration) -> Result<(), orbit_common::OrbitError> {
            let mut state = self.state.borrow_mut();
            state.waits += 1;
            // Exit/reuse during the TERM grace interval.
            if (self.change_at == 2 || self.change_at == 4) && state.waits == 1 {
                state.generation = 1;
            }
            Ok(())
        }
    }
    let token = format!("{STABLE_TOKEN_PREFIX}pidns=-:Sun Aug  2 20:13:45 2026");
    for provider in [false, true] {
        for grouped in [false, true] {
            for (change_at, ignore_term, expected_signals) in [
                (0, true, vec![]),               // after initial verification, before binding
                (1, true, vec![]),               // bound original exits just before TERM
                (2, true, vec![libc::SIGTERM]),  // reuse during grace
                (3, false, vec![libc::SIGTERM]), // ordinary TERM
                (3, true, vec![libc::SIGTERM, libc::SIGKILL]), // escalation
                (4, true, vec![libc::SIGTERM]),  // unbound/reused group remains
                (5, true, vec![libc::SIGTERM]),  // exit/reuse just before KILL
            ] {
                let state = Rc::new(RefCell::new(State::default()));
                let bind = |_, _: Option<&str>| {
                    if change_at == 0 {
                        state.borrow_mut().generation = 1;
                        return Ok(None);
                    }
                    Ok(Some(Target {
                        state: state.clone(),
                        change_at,
                        grouped,
                        ignore_term,
                    }))
                };
                if provider {
                    let result = stop_provider_process_with_binding(
                        43210,
                        Some(&token),
                        |_, _| ProcessLiveness::Alive,
                        |_| ProbeOutcome::Token(token.clone()),
                        bind,
                    );
                    if grouped && change_at == 4 {
                        assert!(
                            matches!(
                                result,
                                Err(orbit_common::OrbitError::RunCancellationIncomplete {
                                    group_alive: true,
                                    leader_alive: false,
                                    ..
                                })
                            ),
                            "an uncertain group must never be reported stopped"
                        );
                    } else {
                        let result = result.unwrap();
                        assert_eq!(
                            result,
                            if change_at == 0 {
                                ProviderStop::NotRunning
                            } else {
                                ProviderStop::Stopped
                            }
                        );
                    }
                } else {
                    let result = signal_run_owner_with_binding(
                        &owner_run(Some(43210), Some(token.clone())),
                        OwnerIdentity::Verified,
                        bind,
                    );
                    if grouped && change_at == 4 {
                        assert!(
                            matches!(
                                result,
                                Err(orbit_common::OrbitError::RunCancellationIncomplete {
                                    group_alive: true,
                                    leader_alive: false,
                                    ..
                                })
                            ),
                            "an uncertain group must never be reported stopped"
                        );
                    } else {
                        let result = result.unwrap();
                        assert_eq!(
                            result,
                            match (change_at, ignore_term, grouped) {
                                (0, _, _) => "already_exited",
                                (3 | 5, true, true) => "killed_process_group",
                                (3 | 5, true, false) => "killed_owner",
                                (_, _, true) => "terminated_process_group",
                                (_, _, false) => "terminated_owner",
                            }
                        );
                    }
                }
                assert_eq!(
                    state.borrow().signals,
                    expected_signals
                        .into_iter()
                        .map(|signal| (0, signal))
                        .collect::<Vec<_>>(),
                    "replacement must never receive TERM/KILL: provider={provider}, group={grouped}, boundary={change_at}"
                );
            }
        }
    }
    // Even a matching second-resolution token must not adopt the PID if its
    // kernel-resolution identity changed since the initial verification.
    let replacement = orbit_common::test_env::spawn_unrelated_process();
    let ProbeOutcome::Token(token) = probe_process_start_identity(replacement.pid()) else {
        panic!("replacement identity must be readable");
    };
    let mut recorded_key =
        orbit_common::process::ancestry::process_start_key(replacement.pid()).unwrap();
    recorded_key.starttime ^= 1;
    assert!(
        super::super::owner::BoundTarget::bind(replacement.pid(), Some(&token), Some(recorded_key))
            .unwrap()
            .is_none()
    );
    assert!(
        process_is_alive(replacement.pid()),
        "binding must refuse a changed kernel identity even when coarse tokens match"
    );
}
