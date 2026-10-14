//! A process joins each authority root once, however many stores and
//! registries it opens: every later process join that the live one covers
//! shares it instead of writing and fsyncing another participant record
//! [ORB-15674]. Sharing does not let a process start work behind a pending
//! generation switch. A writer whose join creates the root records the
//! executable digest it hashed, so later commands reuse it [ORB-16059].
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::fs::generation::{
    Access, CompatibilityIdentity, GenerationGuard, LedgerCompatibility, Participant,
    ParticipantRole, pending_switch,
};
use orbit_common::test_env;

fn identity(version: u32, writer_floor: u32) -> CompatibilityIdentity {
    let ledger = |version, writer_floor| LedgerCompatibility {
        version,
        writer_floor,
        reader_floor: 0,
    };
    CompatibilityIdentity {
        store_schema: ledger(version, writer_floor),
        task_registry: ledger(1, 0),
        workspace_layout: ledger(1, 0),
        features: BTreeMap::new(),
        feature_floors: BTreeMap::new(),
    }
}

fn process_join(
    root: &Path,
    identity: &CompatibilityIdentity,
    role: ParticipantRole,
    access: Access,
) -> Result<GenerationGuard, orbit_common::OrbitError> {
    GenerationGuard::for_process(root, identity, role, None, access, false, || Ok(10))
}

/// Participant records under `root`, live or not yet collected.
fn participant_records(root: &Path) -> usize {
    std::fs::read_dir(root.join(".generation-participants"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count()
        })
        .unwrap_or(0)
}

/// The image digests remembered under `root`.
fn remembered_digests(root: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(root.join(".generation-image-digest.json")) else {
        return Vec::new();
    };
    let cache: serde_json::Value = serde_json::from_str(&raw).expect("the digest cache is JSON");
    cache["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["sha256"].as_str().map(str::to_string))
        .collect()
}

/// Whether this process is the fresh child that runs `test`. Otherwise this
/// re-executes `test` alone in one and returns `false` once it has passed.
///
/// A process's first join fixes its executable digest for good, so only a
/// process no other test has joined from shows what a first join records.
fn in_fresh_process(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_GENERATION_PARTICIPATION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let image = std::env::current_exe().expect("test image");
    await_settled_image(&image);
    let mut command = std::process::Command::new(image);
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test);
    let logs = tempfile::tempdir().expect("child logs");
    let output = test_env::run_child_test(&mut command, test, logs.path());
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

/// An image changed in the last two seconds is never remembered, and a test
/// can start sooner than that after its binary is linked.
fn await_settled_image(image: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::time::{SystemTime, UNIX_EPOCH};
        let meta = std::fs::metadata(image).expect("test image metadata");
        let changed = UNIX_EPOCH
            + Duration::new(
                u64::try_from(meta.ctime()).unwrap_or(0),
                u32::try_from(meta.ctime_nsec()).unwrap_or(0),
            );
        let settled = changed + Duration::from_millis(2_500);
        if let Ok(wait) = settled.duration_since(SystemTime::now()) {
            std::thread::sleep(wait);
        }
    }
    #[cfg(not(unix))]
    let _ = image;
}

#[test]
fn a_process_registers_once_however_many_times_it_joins_a_root() {
    if !in_fresh_process("a_process_registers_once_however_many_times_it_joins_a_root") {
        return;
    }
    let authority = tempfile::tempdir().expect("authority");
    // The first join creates the root.
    let root = &authority.path().join("orbit");
    let identity = identity(10, 0);

    let writer = process_join(root, &identity, ParticipantRole::Command, Access::Write)
        .expect("the process joins");
    #[cfg(unix)]
    assert_eq!(
        remembered_digests(root),
        vec![
            orbit_common::fs::generation::executable_generation(
                &std::env::current_exe().expect("test image")
            )
            .expect("digest of the test image")
        ],
        "a first writer records the digest it hashed before its join created the root, so \
         later commands against the root do not hash the image again"
    );
    let opens = (0..10)
        .map(|open| {
            let access = if open % 2 == 0 {
                Access::ReadOnly
            } else {
                Access::Write
            };
            process_join(root, &identity, ParticipantRole::Command, access)
                .expect("a later open shares the join")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        participant_records(root),
        1,
        "one record for the process, not one per open"
    );
    assert!(
        opens.iter().all(|open| !open.joined_foreign_generation()),
        "a shared open joins the generation this process recorded"
    );

    drop(writer);
    assert_eq!(
        participant_records(root),
        1,
        "the join stays registered while any open still holds it"
    );
    drop(opens);
    assert_eq!(
        participant_records(root),
        0,
        "the last guard withdraws the record"
    );

    let _rejoined = process_join(root, &identity, ParticipantRole::Command, Access::ReadOnly)
        .expect("a released join can be made again");
    assert_eq!(participant_records(root), 1);

    let other = authority.path().join("other");
    let _other = process_join(&other, &identity, ParticipantRole::Command, Access::Write)
        .expect("the process joins a second root");
    assert!(
        remembered_digests(&other).is_empty(),
        "a digest hashed for one root is never recorded under another"
    );
}

/// A writer must be visible to the next newcomer's compatibility check, so a
/// reader's join never stands in for one.
#[test]
fn a_read_only_join_does_not_cover_a_writer() {
    if !in_fresh_process("a_read_only_join_does_not_cover_a_writer") {
        return;
    }
    let authority = tempfile::tempdir().expect("authority");
    let root = &authority.path().join("orbit");
    let identity = identity(10, 0);

    let _reader = process_join(root, &identity, ParticipantRole::Command, Access::ReadOnly)
        .expect("the reader joins");
    assert!(
        !root.join(".generation-image-digest.json").exists(),
        "a read-only first join hashes the image but writes no digest cache"
    );
    let _writer = process_join(root, &identity, ParticipantRole::Command, Access::Write)
        .expect("the writer joins");
    assert_eq!(participant_records(root), 2);
    let _covered = process_join(root, &identity, ParticipantRole::Command, Access::ReadOnly)
        .expect("a later reader shares a join");
    assert_eq!(participant_records(root), 2);
}

/// A live join already holds a breaking upgrade back, but a long-lived
/// process must not start new work behind it: an open that would have
/// shared the join is refused, as a new join is.
#[test]
fn a_pending_switch_refuses_an_open_that_would_share_the_join() {
    let root = tempfile::tempdir().expect("authority");
    let root = root.path().to_path_buf();
    let old = identity(10, 0);
    let live = process_join(&root, &old, ParticipantRole::Dashboard, Access::Write)
        .expect("the dashboard joins");

    let upgrader = std::thread::spawn({
        let root = root.clone();
        move || {
            let digest = format!("{:064x}", 12);
            let identity = identity(12, 12);
            GenerationGuard::join(
                &root,
                &Participant {
                    digest: &digest,
                    identity: &identity,
                    role: ParticipantRole::Command,
                    access: Access::Write,
                    handover: None,
                    in_activity: false,
                },
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

    let refused = match process_join(&root, &old, ParticipantRole::Dashboard, Access::ReadOnly) {
        Ok(_guard) => panic!("a pending switch refuses an open behind it"),
        Err(error) => error.to_string(),
    };
    assert!(
        refused.contains("generation switch is pending"),
        "{refused}"
    );

    drop(live);
    let upgraded = upgrader
        .join()
        .expect("upgrader thread")
        .expect("the breaking writer takes over once the dashboard yields");
    assert!(pending_switch(&root).is_none());
    drop(upgraded);
}
