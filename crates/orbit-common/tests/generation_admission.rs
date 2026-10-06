//! Generation admission forgets participants that have exited, admits ordinary
//! startups in parallel, and lets exclusive waiters progress under startup load.
//!
//! `.generation-compat.json` is the envelope of identities admitted since the
//! authority last had no holder of `.generation.lock`. A guard drop does not
//! rewrite that file; the next join must, or a departed writer keeps refusing
//! newcomers and forcing live, compatible processes to yield.
//!
//! Joins that write nothing shared hold admission shared, so concurrent
//! startups are admitted side by side; a breaking upgrade or an update still
//! excludes them. A join that cannot get admission within its bound says
//! whether an upgrade held it or other startups did.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use fs2::FileExt;
use orbit_common::fs::generation::{
    Access, CompatibilityIdentity, GenerationGuard, GenerationUpdate, LedgerCompatibility,
    Participant, ParticipantRole, pending_switch,
};
use serde_json::{Value, json};

fn digest(n: u64) -> String {
    format!("{n:064x}")
}

fn identity(version: u32, writer_floor: u32) -> CompatibilityIdentity {
    CompatibilityIdentity {
        store_schema: LedgerCompatibility {
            version,
            writer_floor,
            reader_floor: 0,
        },
        workspace_layout: LedgerCompatibility {
            version: 1,
            writer_floor: 0,
            reader_floor: 0,
        },
        features: BTreeMap::new(),
    }
}

fn join(
    root: &Path,
    digest: &str,
    identity: &CompatibilityIdentity,
    role: ParticipantRole,
    access: Access,
) -> Result<GenerationGuard, orbit_common::OrbitError> {
    let participant = Participant {
        digest,
        identity,
        role,
        access,
    };
    // A zero bound fails immediately if join decides to quiesce. Success
    // therefore means no live participant was asked to yield.
    GenerationGuard::join(root, &participant, Duration::ZERO, || Ok(0))
}

fn compat(root: &Path) -> Value {
    let raw = std::fs::read_to_string(root.join(".generation-compat.json")).expect("compat record");
    serde_json::from_str(&raw).expect("compat json")
}

fn assert_envelope(root: &Path, store: Value, writer_min_layout: Option<u32>) {
    let record = compat(root);
    let lock = std::fs::read_to_string(root.join(".generation.lock")).expect("generation lock");
    let recorded = lock
        .strip_prefix("1:")
        .and_then(|body| body.strip_suffix('\n'))
        .expect("v1 generation record");
    assert_eq!(record["contract"], "compatibility-generation-v2");
    assert_eq!(record["record_digest"], recorded);
    assert_eq!(
        record["envelope"],
        json!({
            "store_schema": store,
            "workspace_layout": {
                "min_version": 1,
                "max_version": 1,
                "max_reader_floor": 0,
                "max_writer_floor": 0,
                "writer_min_version": writer_min_layout,
            },
            "features": {}
        })
    );
}

fn store(
    min_version: u32,
    max_version: u32,
    max_writer_floor: u32,
    writer_min_version: Option<u32>,
) -> Value {
    json!({
        "min_version": min_version,
        "max_version": max_version,
        "max_reader_floor": 0,
        "max_writer_floor": max_writer_floor,
        "writer_min_version": writer_min_version,
    })
}

#[test]
fn exited_writer_does_not_refuse_a_later_reader() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let old = identity(10, 0);
    let old_guard = join(
        root,
        &digest(10),
        &old,
        ParticipantRole::Command,
        Access::Write,
    )
    .expect("v10 writer joins");
    assert_envelope(root, store(10, 10, 0, Some(10)), Some(1));
    drop(old_guard);

    let reader = identity(12, 12);
    let reader_guard = join(
        root,
        &digest(12),
        &reader,
        ParticipantRole::Command,
        Access::ReadOnly,
    )
    .expect("read-only v12 joins after the v10 writer has exited");
    assert_envelope(root, store(12, 12, 12, None), None);
    drop(reader_guard);
}

#[test]
fn exited_writer_does_not_force_a_compatible_live_participant_to_yield() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let old = identity(10, 0);
    drop(
        join(
            root,
            &digest(10),
            &old,
            ParticipantRole::Command,
            Access::Write,
        )
        .expect("v10 writer joins"),
    );

    let mid = identity(11, 0);
    let live = join(
        root,
        &digest(11),
        &mid,
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("additive v11 writer joins an empty authority");
    assert_envelope(root, store(11, 11, 0, Some(11)), Some(1));
    assert!(
        pending_switch(root).is_none(),
        "reseeding an empty authority records no switch"
    );

    let newer = identity(12, 11);
    let admitted = join(
        root,
        &digest(12),
        &newer,
        ParticipantRole::Command,
        Access::Write,
    )
    .expect("v12 writer joins beside the live v11 process");
    assert!(
        pending_switch(root).is_none(),
        "a departed v10 writer must not ask the live v11 process to yield"
    );
    assert_envelope(root, store(11, 12, 11, Some(11)), Some(1));
    drop(admitted);
    drop(live);
}

#[test]
fn live_writer_is_not_treated_as_an_empty_authority() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let old = identity(10, 0);
    let live = join(
        root,
        &digest(10),
        &old,
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("v10 writer holds the authority");

    let breaking = identity(12, 12);
    let refused = match join(
        root,
        &digest(12),
        &breaking,
        ParticipantRole::Command,
        Access::Write,
    ) {
        Ok(_guard) => panic!("a live v10 writer still refuses a breaking v12 writer"),
        Err(error) => error.to_string(),
    };
    assert!(refused.contains("live writer at version 10"), "{refused}");
    assert!(refused.contains("did not yield"), "{refused}");
    assert!(pending_switch(root).is_none(), "{refused}");
    assert_envelope(root, store(10, 10, 0, Some(10)), Some(1));
    drop(live);
}

fn join_within(
    root: &Path,
    digest: &str,
    identity: &CompatibilityIdentity,
    access: Access,
    bound: Duration,
    store_schema: impl FnOnce() -> Result<u32, orbit_common::OrbitError>,
) -> Result<GenerationGuard, orbit_common::OrbitError> {
    let participant = Participant {
        digest,
        identity,
        role: ParticipantRole::Command,
        access,
    };
    GenerationGuard::join(root, &participant, bound, store_schema)
}

/// Start `count` joins at once and return each outcome with the elapsed time.
fn concurrent_joins<F>(count: usize, join: F) -> (Vec<Result<GenerationGuard, String>>, Duration)
where
    F: Fn(usize) -> Result<GenerationGuard, orbit_common::OrbitError> + Send + Sync + 'static,
{
    let join = Arc::new(join);
    let start = Arc::new(Barrier::new(count + 1));
    let handles = (0..count)
        .map(|index| {
            let (join, start) = (Arc::clone(&join), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                join(index).map_err(|error| error.to_string())
            })
        })
        .collect::<Vec<_>>();
    start.wait();
    let began = Instant::now();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("join thread"))
        .collect();
    (outcomes, began.elapsed())
}

#[test]
fn concurrent_readers_of_a_v1_record_read_the_store_schema_in_parallel() {
    const JOINERS: usize = 32;
    const SCHEMA_READ: Duration = Duration::from_millis(250);
    let root = tempfile::tempdir().expect("authority");
    let root = root.path().to_path_buf();
    // A v1 process owns the record, so each reader consults the store schema.
    std::fs::write(root.join(".generation.lock"), format!("1:{}\n", digest(1)))
        .expect("v1 generation record");
    let reader = identity(5, 0);

    let (outcomes, elapsed) = concurrent_joins(JOINERS, {
        let root = root.clone();
        move |_| {
            // A zero bound refuses at once if any join waited for admission.
            join_within(
                &root,
                &digest(2),
                &reader,
                Access::ReadOnly,
                Duration::ZERO,
                || {
                    std::thread::sleep(SCHEMA_READ);
                    Ok(5)
                },
            )
        }
    });

    for outcome in &outcomes {
        let guard = outcome
            .as_ref()
            .expect("every concurrent reader is admitted");
        assert!(guard.joined_foreign_generation());
    }
    let serial = SCHEMA_READ * JOINERS as u32;
    assert!(
        elapsed < serial / 4,
        "{JOINERS} joins took {elapsed:?}; serialised schema reads would take {serial:?}"
    );
}

#[test]
fn concurrent_compatible_joins_beside_a_live_writer_never_wait() {
    const JOINERS: usize = 32;
    let root = tempfile::tempdir().expect("authority");
    let root = root.path().to_path_buf();
    let live_identity = identity(10, 0);
    let live = join(
        &root,
        &digest(10),
        &live_identity,
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("v10 writer holds the authority");

    let (outcomes, _) = concurrent_joins(JOINERS, {
        let root = root.clone();
        move |index| {
            let access = if index % 2 == 0 {
                Access::Write
            } else {
                Access::ReadOnly
            };
            join_within(
                &root,
                &digest(10),
                &identity(10, 0),
                access,
                Duration::ZERO,
                || Ok(10),
            )
        }
    });

    for outcome in &outcomes {
        assert!(
            outcome.is_ok(),
            "{outcome:?}",
            outcome = outcome.as_ref().err()
        );
    }
    assert_envelope(&root, store(10, 10, 0, Some(10)), Some(1));
    drop(outcomes);
    drop(live);
}

#[test]
fn envelope_widening_progresses_beside_a_continuous_shared_join_stream() {
    const JOINERS: usize = 32;
    let root = tempfile::tempdir().expect("authority");
    let root = root.path().to_path_buf();
    let live = join(
        &root,
        &digest(10),
        &identity(10, 0),
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("v10 writer holds the authority");
    // Model an in-flight shared admission that takes time to finish, while
    // real startups keep replenishing the shared holders around it.
    let in_flight = OpenOptions::new()
        .read(true)
        .open(root.join(".generation-admission.lock"))
        .expect("admission lock");
    in_flight.lock_shared().expect("in-flight shared admission");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready, started) = sync_channel(JOINERS);
    let streams = (0..JOINERS)
        .map(|_| {
            let (root, stop, ready) = (root.clone(), Arc::clone(&stop), ready.clone());
            std::thread::spawn(move || {
                let mut count = 0;
                loop {
                    drop(
                        join_within(
                            &root,
                            &digest(10),
                            &identity(10, 0),
                            Access::ReadOnly,
                            Duration::from_secs(10),
                            || Ok(10),
                        )
                        .expect("ordinary startups in the stream are admitted"),
                    );
                    count += 1;
                    if count == 1 {
                        ready.send(()).expect("stream started");
                    }
                    if stop.load(Ordering::Acquire) {
                        return count;
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    drop(ready);
    for _ in 0..JOINERS {
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("every shared stream has started");
    }
    let widening = std::thread::spawn({
        let root = root.clone();
        move || {
            join_within(
                &root,
                &digest(11),
                &identity(11, 0),
                Access::Write,
                Duration::from_secs(3),
                || Ok(10),
            )
            .map_err(|error| error.to_string())
        }
    });
    std::thread::sleep(Duration::from_millis(200));
    drop(in_flight);
    let outcome = widening.join().expect("widening thread");
    stop.store(true, Ordering::Release);
    for stream in streams {
        assert!(stream.join().expect("shared stream") > 0);
    }
    let widened = outcome.expect(
        "an ordinary envelope-widening startup must progress while shared startups keep arriving",
    );
    assert_envelope(&root, store(10, 11, 0, Some(10)), Some(1));
    assert!(pending_switch(&root).is_none());
    drop(widened);
    drop(live);
}

#[test]
fn timed_out_and_abandoned_exclusive_waiters_do_not_block_shared_startups() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let live = join(
        root,
        &digest(10),
        &identity(10, 0),
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("live v10 writer");
    let in_flight = OpenOptions::new()
        .read(true)
        .open(root.join(".generation-admission.lock"))
        .expect("admission lock");
    in_flight.lock_shared().expect("in-flight admission");
    let outcome = join_within(
        root,
        &digest(11),
        &identity(11, 0),
        Access::Write,
        Duration::from_millis(100),
        || Ok(10),
    );
    assert!(outcome.is_err(), "a held admission outlives the bound");
    // An exited waiter's file survives but its OS lock does not. Liveness
    // must be decided by that lock, without waiting for a lease to expire.
    std::fs::write(
        root.join(".generation-admission-waiters/abandoned.waiting"),
        [],
    )
    .expect("abandoned waiter record");
    let reader = join_within(
        root,
        &digest(10),
        &identity(10, 0),
        Access::ReadOnly,
        Duration::ZERO,
        || Ok(10),
    )
    .expect("neither timed-out nor abandoned waiters delay shared startups");
    drop(reader);
    drop(in_flight);
    let widened = join_within(
        root,
        &digest(11),
        &identity(11, 0),
        Access::Write,
        Duration::from_secs(3),
        || Ok(10),
    )
    .expect("a later exclusive join can progress too");
    assert_envelope(root, store(10, 11, 0, Some(10)), Some(1));
    drop(widened);
    drop(live);
}

#[test]
fn contended_admission_waits_for_the_bound_then_names_contention() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".generation-admission.lock"))
        .expect("admission lock");
    held.lock_exclusive().expect("hold admission");

    // Longer than the point where a wait asks whether an upgrade holds it.
    let bound = Duration::from_millis(1500);
    let began = Instant::now();
    let refused = match join_within(
        root,
        &digest(10),
        &identity(10, 0),
        Access::Write,
        bound,
        || Ok(10),
    ) {
        Ok(_guard) => panic!("a held admission lock refuses past the bound"),
        Err(error) => error.to_string(),
    };
    assert!(began.elapsed() >= bound, "waited the configured bound");
    assert!(refused.contains("contended"), "{refused}");
    assert!(refused.contains("no upgrade pending"), "{refused}");
    assert!(!refused.contains("upgrade is"), "{refused}");
}

#[test]
fn an_update_holding_admission_refuses_joins_promptly_as_an_upgrade_in_progress() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path();
    let update = GenerationUpdate::acquire(root).expect("updater takes the authority");

    let began = Instant::now();
    let refused = match join_within(
        root,
        &digest(10),
        &identity(10, 0),
        Access::ReadOnly,
        Duration::from_secs(60),
        || Ok(10),
    ) {
        Ok(_guard) => panic!("an update excludes ordinary joins"),
        Err(error) => error.to_string(),
    };
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "a join is refused during an update, not queued for the whole bound"
    );
    assert!(refused.contains("upgrade is in progress"), "{refused}");
    assert!(!refused.contains("contended"), "{refused}");
    drop(update);
}

#[test]
fn a_breaking_upgrade_refuses_ordinary_joins_until_live_participants_yield() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path().to_path_buf();
    let old = identity(10, 0);
    let live = join(
        &root,
        &digest(10),
        &old,
        ParticipantRole::McpServe,
        Access::Write,
    )
    .expect("v10 writer holds the authority");

    let upgrader = std::thread::spawn({
        let root = root.clone();
        move || {
            join_within(
                &root,
                &digest(12),
                &identity(12, 12),
                Access::Write,
                Duration::from_secs(30),
                || Ok(10),
            )
            .map_err(|error| error.to_string())
        }
    });
    let waiting = Instant::now();
    while pending_switch(&root).is_none() {
        assert!(
            waiting.elapsed() < Duration::from_secs(10),
            "the breaking writer records a pending switch"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let refused = match join(
        &root,
        &digest(10),
        &old,
        ParticipantRole::Command,
        Access::ReadOnly,
    ) {
        Ok(_guard) => panic!("a pending switch refuses an old-identity newcomer"),
        Err(error) => error.to_string(),
    };
    assert!(
        refused.contains("generation switch is pending"),
        "{refused}"
    );
    assert!(!refused.contains("contended"), "{refused}");
    assert!(
        GenerationUpdate::acquire(&root).is_err(),
        "an update is refused while a switch is pending"
    );

    drop(live);
    let upgraded = upgrader
        .join()
        .expect("upgrader thread")
        .expect("the breaking writer takes over once the live writer yields");
    assert!(pending_switch(&root).is_none());
    assert_envelope(&root, store(12, 12, 12, Some(12)), Some(1));
    drop(upgraded);
}
