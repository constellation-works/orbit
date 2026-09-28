use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{RecvTimeoutError, sync_channel},
};
use std::time::Duration;

use super::scoped;

/// Two `scoped` guards contending for the same variable must be serialized:
/// the contender cannot enter while the holder is alive, never observes the
/// holder's temporary value, and restores the pre-existing value rather than
/// the holder's. Admission order is proven by a flag the holder sets just
/// before releasing, not by timing; the bounded waits only keep a regression
/// (missing exclusion or a lock that is never released) from hanging the run.
#[test]
fn scoped_guard_excludes_a_contending_guard_until_the_holder_releases() {
    const VAR: &str = "ORBIT_TEST_ENV_CONTENTION_PROBE";
    const HOLDER_VALUE: &str = "held-by-first-guard";
    const CONTENDER_VALUE: &str = "requested-by-contender";
    // How long the holder keeps the lock after the contender starts
    // acquiring; an unserialized guard is admitted well within it.
    const CONTENTION_WINDOW: Duration = Duration::from_millis(250);
    const BOUND: Duration = Duration::from_secs(10);

    #[derive(Debug)]
    struct Admission {
        holder_released: bool,
        observed: Option<String>,
    }

    let baseline = std::env::var(VAR).ok();
    let holder_released = Arc::new(AtomicBool::new(false));
    let (attempting_tx, attempting_rx) = sync_channel::<()>(1);
    let (admitted_tx, admitted_rx) = sync_channel::<Admission>(1);

    let holder = scoped([(VAR, Some(HOLDER_VALUE))]);

    let contender = std::thread::spawn({
        let holder_released = Arc::clone(&holder_released);
        move || {
            attempting_tx
                .send(())
                .expect("holder is waiting for the attempt");
            let guard = scoped([(VAR, Some(CONTENDER_VALUE))]);
            // Capacity-1 channel with a single send: never blocks while the
            // guard is held, so the holder cannot deadlock against it.
            admitted_tx
                .send(Admission {
                    holder_released: holder_released.load(Ordering::SeqCst),
                    observed: std::env::var(VAR).ok(),
                })
                .expect("holder is waiting for admission");
            drop(guard);
        }
    });

    attempting_rx
        .recv_timeout(BOUND)
        .expect("contender thread never started acquiring its guard");
    match admitted_rx.recv_timeout(CONTENTION_WINDOW) {
        Err(RecvTimeoutError::Timeout) => {}
        Ok(admission) => {
            panic!("second guard was admitted while the first was still held: {admission:?}")
        }
        Err(RecvTimeoutError::Disconnected) => {
            panic!("contender exited without being admitted")
        }
    }
    assert_eq!(
        std::env::var(VAR).ok().as_deref(),
        Some(HOLDER_VALUE),
        "a blocked contender must not overwrite the holder's value"
    );

    holder_released.store(true, Ordering::SeqCst);
    drop(holder);

    let admission = admitted_rx
        .recv_timeout(BOUND)
        .expect("contender was never admitted after the holder released its guard");
    assert!(
        admission.holder_released,
        "contender was admitted before the holder released its guard"
    );
    assert_eq!(
        admission.observed.as_deref(),
        Some(CONTENDER_VALUE),
        "contender must see only the value it requested"
    );

    contender.join().expect("contender thread panicked");
    assert_eq!(
        std::env::var(VAR).ok(),
        baseline,
        "both guards must restore the pre-existing value, not the holder's temporary one"
    );
}

/// A test that panics while holding the guard must not turn the shared lock
/// into a permanent `PoisonError` for every later test in the binary
/// (ORB-11079 / F2026-08-098): `scoped` recovers a poisoned lock instead of
/// propagating it, and the panicking guard's own `Drop` still restores the
/// environment during unwinding.
#[test]
fn scoped_guard_recovers_after_a_sibling_assertion_panics_while_holding_it() {
    const VAR: &str = "ORBIT_TEST_ENV_POISON_PROBE";
    let baseline = std::env::var(VAR).ok();

    let panicked = std::thread::spawn(|| {
        let _guard = scoped([(VAR, Some("from-panicking-assertion"))]);
        panic!("simulated managed-run assertion failure while holding the env guard");
    })
    .join();
    assert!(panicked.is_err(), "expected the spawned thread to panic");

    // The shared mutex is now poisoned. A well-behaved next guard must
    // recover it rather than cascading the poison into every remaining test,
    // and must see only what it itself requested — not a leak from the
    // panicking thread's now-unwound scope.
    let guard = scoped([(VAR, Some("isolated-after-recovery"))]);
    assert_eq!(
        std::env::var(VAR).ok(),
        Some("isolated-after-recovery".to_string())
    );
    drop(guard);

    assert_eq!(std::env::var(VAR).ok(), baseline);
}

/// The in-process managed-run envelope and the subprocess scrub must not
/// drift apart: anything worth hiding from a same-process test is worth
/// hiding from a spawned `orbit` child, which re-reads the environment from
/// scratch and can route durable writes with it (ORB-11300).
#[test]
fn inherited_authority_covers_the_managed_run_envelope_without_duplicates() {
    for name in super::MANAGED_RUN_ENV {
        assert!(
            super::INHERITED_AUTHORITY_ENV.contains(name),
            "{name} is scrubbed in-process but would still reach a spawned child"
        );
    }

    let mut seen = super::INHERITED_AUTHORITY_ENV.to_vec();
    seen.sort_unstable();
    let deduped = {
        let mut deduped = seen.clone();
        deduped.dedup();
        deduped
    };
    assert_eq!(seen, deduped, "duplicate names hide an editing mistake");
}

/// `clear_inherited_authority` must hand the caller every name exactly once —
/// a fixture wires it straight into `Command::env_remove`.
#[test]
fn clear_inherited_authority_visits_every_name_once() {
    let mut cleared = Vec::new();
    super::clear_inherited_authority(|name| cleared.push(name.to_string()));

    assert_eq!(cleared.len(), super::INHERITED_AUTHORITY_ENV.len());
    for name in super::INHERITED_AUTHORITY_ENV {
        assert!(cleared.iter().any(|seen| seen == name), "missing {name}");
    }
}

/// An inherited `ORBIT_PLUGIN_BROKER` forwards a fixture's plugin calls to the
/// enclosing run's host broker, which refuses them (F2026-09-244). The shared
/// clear must strip it from a child command, while a value the fixture sets
/// afterwards on purpose still reaches the child.
#[test]
fn clear_inherited_authority_strips_the_plugin_broker_socket_from_a_child() {
    const BROKER: &str = "ORBIT_PLUGIN_BROKER";
    let child_value = |command: &std::process::Command| {
        command
            .get_envs()
            .find(|(name, _)| *name == BROKER)
            .map(|(_, value)| value.map(|value| value.to_os_string()))
    };

    let mut inherited = std::process::Command::new("orbit");
    inherited.env(BROKER, "/run/orbit/live-host-broker.sock");
    super::clear_inherited_authority(|name| {
        inherited.env_remove(name);
    });
    assert_eq!(
        child_value(&inherited),
        Some(None),
        "an inherited broker socket must be removed from the child"
    );

    let mut deliberate = std::process::Command::new("orbit");
    super::clear_inherited_authority(|name| {
        deliberate.env_remove(name);
    });
    deliberate.env(BROKER, "/tmp/fixture-broker.sock");
    assert_eq!(
        child_value(&deliberate),
        Some(Some("/tmp/fixture-broker.sock".into())),
        "a broker the fixture sets after clearing must still reach the child"
    );
}

/// The blocker is the one signal environment-dependent tests skip on, so it
/// must agree with the probe they would otherwise rely on: `None` exactly when
/// the current process can derive its own versioned token.
#[test]
fn start_identity_probe_blocker_agrees_with_the_probe() {
    let token = crate::process::identity::process_start_identity_token(std::process::id());
    match super::start_identity_probe_blocker() {
        None => assert!(
            token.is_some(),
            "no blocker reported but the probe yielded no token"
        ),
        Some(reason) => {
            assert!(
                token.is_none(),
                "blocker reported ({reason}) but the probe yielded a token"
            );
            assert!(!reason.is_empty());
        }
    }
}
