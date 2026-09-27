use std::path::Path;
use std::time::{Duration, Instant};

use crate::fs::generation::{
    Access, CompatibilityIdentity, GenerationGuard, GenerationUpdate, LedgerCompatibility,
    Participant, ParticipantRole, is_clock_generation_hold, pending_switch,
};
use crate::fs::generation::{finish_clock_generation_hold, record_clock_generation_hold};

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const THIRD: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// An identity at store schema `version` whose newest migrations older
/// binaries cannot keep writing / reading are `writer_floor` / `reader_floor`.
fn identity(version: u32, writer_floor: u32, reader_floor: u32) -> CompatibilityIdentity {
    CompatibilityIdentity {
        store_schema: LedgerCompatibility {
            version,
            writer_floor,
            reader_floor,
        },
        workspace_layout: LedgerCompatibility {
            version: 3,
            writer_floor: 3,
            reader_floor: 3,
        },
        features: [("automation".to_string(), 3)].into(),
    }
}

fn join_as(
    root: &Path,
    digest: &str,
    identity: &CompatibilityIdentity,
    role: ParticipantRole,
    access: Access,
    quiesce: Duration,
) -> Result<GenerationGuard, crate::OrbitError> {
    let participant = Participant {
        digest,
        identity,
        role,
        access,
    };
    GenerationGuard::join(root, &participant, quiesce, || {
        Ok(identity.store_schema.version)
    })
}

fn join_writer(
    root: &Path,
    digest: &str,
    identity: &CompatibilityIdentity,
) -> Result<GenerationGuard, crate::OrbitError> {
    join_as(
        root,
        digest,
        identity,
        ParticipantRole::Command,
        Access::Write,
        Duration::ZERO,
    )
}

fn refusal_text(result: Result<GenerationGuard, crate::OrbitError>) -> String {
    match result {
        Ok(_) => panic!("admission should have been refused"),
        Err(error) => error.to_string(),
    }
}

fn read_only_join<F>(
    root: &Path,
    digest: &str,
    compiled_schema: u32,
    store_schema: F,
) -> Result<GenerationGuard, crate::OrbitError>
where
    F: FnOnce() -> Result<u32, crate::OrbitError>,
{
    let identity = identity(compiled_schema, 0, 0);
    let participant = Participant {
        digest,
        identity: &identity,
        role: ParticipantRole::Command,
        access: Access::ReadOnly,
    };
    GenerationGuard::join(root, &participant, Duration::ZERO, store_schema)
}

#[test]
fn clock_generation_hold_coalesces_refused_ticks_until_resumed() {
    use chrono::{Duration, TimeZone, Utc};

    let root = tempfile::tempdir().expect("root");
    let old = GenerationGuard::acquire(root.path(), OLD).expect("old pin");
    let first = Utc
        .with_ymd_and_hms(2026, 9, 26, 1, 28, 0)
        .single()
        .expect("date");
    for tick in 0..15 {
        assert!(GenerationGuard::acquire(root.path(), NEW).is_err());
        record_clock_generation_hold(root.path(), NEW, first + Duration::minutes(tick))
            .expect("record refused tick");
    }
    assert!(
        finish_clock_generation_hold(root.path(), OLD, first)
            .expect("old tick")
            .is_none()
    );
    drop(old);
    let _new = GenerationGuard::acquire(root.path(), NEW).expect("new generation admitted");
    let summary = finish_clock_generation_hold(root.path(), NEW, first + Duration::minutes(15))
        .expect("close hold")
        .expect("one summary");
    assert!(
        summary.contains("started_at=2026-09-26T01:28:00+00:00"),
        "{summary}"
    );
    assert!(
        summary.contains("ended_at=2026-09-26T01:43:00+00:00"),
        "{summary}"
    );
    assert!(summary.contains("refused_ticks=15"), "{summary}");
    assert!(
        finish_clock_generation_hold(root.path(), NEW, first + Duration::minutes(16))
            .expect("second successful tick")
            .is_none()
    );
}

#[test]
fn missing_root_is_created_on_first_pin() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("missing");
    drop(GenerationGuard::acquire(&root, OLD).expect("first pin creates the root"));
    assert!(root.is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
    assert!(root.join(".generation-admission.lock").exists());
}

#[test]
fn file_root_is_refused_without_creating_lock_files() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("not-a-directory");
    std::fs::write(&root, b"file").expect("file root");
    let error = match GenerationGuard::acquire(&root, OLD) {
        Ok(_) => panic!("a file root should be refused"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("generation root must be a directory"),
        "{error}"
    );
    assert!(!parent.path().join(".generation.lock").exists());
    assert!(!parent.path().join(".generation-admission.lock").exists());
}

#[test]
fn root_with_parent_dir_components_pins_the_resolved_directory() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("nested");
    std::fs::create_dir(&root).expect("nested");
    let via_parent = parent.path().join("nested").join("..").join("nested");
    drop(GenerationGuard::acquire(&via_parent, OLD).expect("resolved nested root"));
    assert_eq!(
        std::fs::read_to_string(root.join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
}

#[test]
fn missing_root_with_parent_final_component_is_refused() {
    let parent = tempfile::tempdir().expect("parent");
    let missing = parent.path().join("missing");
    let via_parent = missing.join("..");
    let error = match GenerationGuard::acquire(&via_parent, OLD) {
        Ok(_) => panic!("a missing root ending in '..' should be refused"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("upgrade admission refused"), "{error}");
    assert!(!parent.path().join(".generation.lock").exists());
    assert!(!parent.path().join(".generation-admission.lock").exists());
    assert!(!missing.exists());
}

#[test]
fn candidate_pin_excludes_old_generation_through_convergence() {
    let root = tempfile::tempdir().expect("root");
    let old = GenerationGuard::acquire(root.path(), OLD).expect("old");
    let concurrent = GenerationGuard::acquire(root.path(), OLD).expect("concurrent old");
    assert!(GenerationUpdate::acquire(root.path()).is_err());
    assert!(GenerationGuard::acquire(root.path(), NEW).is_err());
    drop(old);
    assert!(GenerationUpdate::acquire(root.path()).is_err());
    drop(concurrent);
    let candidate = GenerationUpdate::acquire(root.path())
        .expect("quiescent")
        .pin(NEW, None)
        .expect("pin candidate");
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    let convergence = GenerationGuard::acquire(root.path(), NEW).expect("candidate child");
    drop(candidate);
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    drop(convergence);
    assert!(GenerationUpdate::acquire(root.path()).is_ok());
}

#[test]
fn pinned_newer_generation_refuses_old_digest_until_pin_is_dropped() {
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("old"));
    let newer = GenerationUpdate::acquire(root.path())
        .expect("quiescent update")
        .pin(NEW, None)
        .expect("new pin");
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    drop(newer);
    GenerationGuard::acquire(root.path(), OLD).expect("old digest after new pin exits");
}

#[test]
fn invalid_record_is_refused_without_repair_or_truncation() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join(".generation.lock");
    std::fs::write(&path, "2:unknown").expect("corrupt record");
    assert!(GenerationGuard::acquire(root.path(), NEW).is_err());
    assert!(GenerationUpdate::acquire(root.path()).is_err());
    assert_eq!(std::fs::read_to_string(path).expect("record"), "2:unknown");
}

#[test]
fn failed_installation_releases_admission_without_changing_generation() {
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("old"));
    let before = std::fs::read(root.path().join(".generation.lock")).expect("record");
    drop(GenerationUpdate::acquire(root.path()).expect("update"));
    assert_eq!(
        std::fs::read(root.path().join(".generation.lock")).expect("record"),
        before
    );
    GenerationGuard::acquire(root.path(), OLD).expect("old digest after abandoned update");
}

#[test]
fn live_exclusive_holder_in_another_process_blocks_until_reaped() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().expect("root");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    crate::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let mut child = command
        .args([
            "--exact",
            "fs::tests::generation::exclusive_child",
            "--nocapture",
        ])
        .env("ORBIT_TEST_GENERATION_ROOT", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("holder child");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    assert!(lines.any(|line| line.expect("line") == "generation-holder-ready"));
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    child.kill().expect("interrupt isolated holder");
    child.wait().expect("reap holder");
    assert!(GenerationGuard::acquire(root.path(), OLD).is_ok());
}

#[test]
fn second_update_on_same_root_is_refused_while_first_update_is_alive() {
    let root = tempfile::tempdir().expect("root");
    let first = GenerationUpdate::acquire(root.path()).expect("first update");
    assert!(GenerationUpdate::acquire(root.path()).is_err());
    drop(first);
    GenerationUpdate::acquire(root.path()).expect("update after first exits");
}

#[cfg(unix)]
#[test]
fn first_create_on_readonly_root_is_refused_without_creating_lock_files() {
    use std::os::unix::fs::PermissionsExt;
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("readonly");
    std::fs::create_dir(&root).expect("create readonly root");
    let mut permissions = std::fs::metadata(&root).expect("metadata").permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(&root, permissions.clone()).expect("chmod readonly");
    let error = match GenerationGuard::acquire(&root, OLD) {
        Ok(_) => panic!("readonly first-create should be refused"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("upgrade admission refused"),
        "{error}"
    );
    assert!(!root.join(".generation.lock").exists());
    assert!(!root.join(".generation-admission.lock").exists());
    permissions.set_mode(0o755);
    std::fs::set_permissions(&root, permissions).expect("restore writable");
}

#[test]
fn exclusive_child() {
    use std::io::{Read, Write};
    let Some(root) = std::env::var_os("ORBIT_TEST_GENERATION_ROOT") else {
        return;
    };
    let _guard =
        GenerationUpdate::acquire(std::path::Path::new(&root)).expect("exclusive admission");
    writeln!(std::io::stdout(), "generation-holder-ready").expect("ready");
    std::io::stdout().flush().expect("flush");
    let _ = std::io::stdin().read(&mut [0u8; 1]);
}

#[cfg(unix)]
fn chmod(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
}

/// Freeze both admission files so opening them for write is refused, the way a
/// read-only mount or a sandbox that denies writes under the authority root
/// does. Participation must still work through the read-only descriptor.
#[cfg(unix)]
fn freeze_records(root: &std::path::Path) {
    chmod(&root.join(".generation.lock"), 0o444);
    chmod(&root.join(".generation-admission.lock"), 0o444);
}

#[cfg(unix)]
fn thaw_records(root: &std::path::Path) {
    chmod(&root.join(".generation.lock"), 0o644);
    chmod(&root.join(".generation-admission.lock"), 0o644);
}

#[cfg(unix)]
fn refusal_of(result: Result<GenerationGuard, crate::OrbitError>) -> String {
    match result {
        Ok(_) => panic!("admission should have been refused"),
        Err(error) => error.to_string(),
    }
}

#[cfg(unix)]
#[test]
fn readonly_records_admit_the_recorded_generation_without_writing() {
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("record OLD"));
    let record = root.path().join(".generation.lock");
    let before = std::fs::read(&record).expect("record");
    freeze_records(root.path());

    let reader = GenerationGuard::acquire(root.path(), OLD).expect("read-only same-generation pin");
    let concurrent =
        GenerationGuard::acquire(root.path(), OLD).expect("second read-only same-generation pin");
    assert_eq!(std::fs::read(&record).expect("record"), before);
    drop(reader);
    drop(concurrent);
    thaw_records(root.path());
}

/// The macOS regression from the nested sandbox suite: a participant that can
/// only read the record must refuse its takeover outright, not discover the
/// read-only descriptor by writing to it (`EBADF` / "Bad file descriptor").
#[cfg(unix)]
#[test]
fn readonly_records_refuse_a_takeover_instead_of_writing_a_read_only_descriptor() {
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("record OLD"));
    let record = root.path().join(".generation.lock");
    let before = std::fs::read(&record).expect("record");
    freeze_records(root.path());

    let refusal = refusal_of(GenerationGuard::acquire(root.path(), NEW));
    assert!(
        refusal.contains("cannot be written from here"),
        "a read-only record must name its own cause: {refusal}"
    );
    assert!(
        !refusal.contains("Bad file descriptor"),
        "a read-only record must not surface as a descriptor error: {refusal}"
    );
    assert_eq!(std::fs::read(&record).expect("record"), before);
    assert!(GenerationGuard::acquire(root.path(), OLD).is_ok());

    thaw_records(root.path());
    drop(
        GenerationGuard::acquire(root.path(), NEW)
            .expect("a writable record still admits a quiescent takeover"),
    );
}

#[cfg(unix)]
#[test]
fn a_readonly_record_refuses_a_guarded_update_pin() {
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("record OLD"));
    let record = root.path().join(".generation.lock");
    let before = std::fs::read(&record).expect("record");
    freeze_records(root.path());

    // The lock still resolves — a descriptor is all flock needs — but the
    // admission answers up front that it could never record a candidate, so
    // an updater refuses before staging instead of discovering it at pin
    // time, with the executable already replaced.
    let update = GenerationUpdate::acquire(root.path()).expect("read-only admission observation");
    let upfront = match update.ensure_can_record() {
        Ok(()) => panic!("a read-only record must refuse before anything is staged"),
        Err(error) => error.to_string(),
    };
    assert!(upfront.contains("cannot be written from here"), "{upfront}");
    let refusal = refusal_of(update.pin(NEW, None));
    assert!(refusal.contains("cannot be written from here"), "{refusal}");
    assert_eq!(std::fs::read(&record).expect("record"), before);

    thaw_records(root.path());
    let writable = GenerationUpdate::acquire(root.path()).expect("writable admission");
    writable
        .ensure_can_record()
        .expect("a writable record can record a candidate");
}

#[test]
fn an_empty_record_is_first_pinned_without_repairing_a_malformed_one() {
    let root = tempfile::tempdir().expect("root");
    let record = root.path().join(".generation.lock");
    std::fs::write(&record, "").expect("empty record");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("first pin over an empty record"));
    assert_eq!(
        std::fs::read_to_string(&record).expect("record"),
        format!("1:{OLD}\n")
    );

    for malformed in ["1:short\n", "1:zz\n", OLD, &format!("1:{OLD}")] {
        std::fs::write(&record, malformed).expect("malformed record");
        assert!(
            GenerationGuard::acquire(root.path(), OLD).is_err(),
            "malformed record {malformed:?} must be refused"
        );
        assert!(GenerationUpdate::acquire(root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(&record).expect("record"),
            malformed,
            "a refused participant must not repair {malformed:?}"
        );
    }
}

#[test]
fn a_live_shared_reader_refuses_a_cross_process_update_until_it_exits() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("record OLD"));
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    crate::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let mut child = command
        .args([
            "--exact",
            "fs::tests::generation::shared_child",
            "--nocapture",
        ])
        .env("ORBIT_TEST_GENERATION_ROOT", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("shared reader child");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    assert!(lines.any(|line| line.expect("line") == "generation-reader-ready"));

    // A live same-generation reader admits its own generation and refuses both
    // the updater and a different one, so no installation or migration can run
    // underneath it.
    let joined = GenerationGuard::acquire(root.path(), OLD).expect("join the live reader");
    let refusal = match GenerationUpdate::acquire(root.path()) {
        Ok(_) => panic!("an update must be refused under a live reader"),
        Err(error) => error.to_string(),
    };
    assert!(
        refusal.contains("Orbit clients or commands are still running"),
        "{refusal}"
    );
    assert!(GenerationGuard::acquire(root.path(), NEW).is_err());
    drop(joined);
    assert!(GenerationUpdate::acquire(root.path()).is_err());

    child.kill().expect("interrupt isolated reader");
    child.wait().expect("reap reader");
    assert!(GenerationUpdate::acquire(root.path()).is_ok());
    assert_eq!(
        std::fs::read_to_string(root.path().join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
}

#[test]
fn shared_child() {
    use std::io::{Read, Write};
    let Some(root) = std::env::var_os("ORBIT_TEST_GENERATION_ROOT") else {
        return;
    };
    let _guard =
        GenerationGuard::acquire(std::path::Path::new(&root), OLD).expect("shared participation");
    writeln!(std::io::stdout(), "generation-reader-ready").expect("ready");
    std::io::stdout().flush().expect("flush");
    let _ = std::io::stdin().read(&mut [0u8; 1]);
}

#[test]
fn read_only_join_keeps_the_recorded_generation_and_blocks_update() {
    let root = tempfile::tempdir().expect("root");
    let old = GenerationGuard::acquire(root.path(), OLD).expect("record OLD");
    let joined =
        read_only_join(root.path(), NEW, 22, || Ok(22)).expect("same-schema read-only join");
    assert!(joined.joined_foreign_generation());
    assert_eq!(
        std::fs::read_to_string(root.path().join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
    let update = match GenerationUpdate::acquire(root.path()) {
        Ok(_) => panic!("update must stay refused under a read-only joiner"),
        Err(error) => error.to_string(),
    };
    assert!(
        update.contains("Orbit clients or commands are still running"),
        "{update}"
    );
    let writes = match GenerationGuard::acquire(root.path(), NEW) {
        Ok(_) => panic!("a writing command must not take over under a live pin"),
        Err(error) => error.to_string(),
    };
    assert!(
        writes.contains("this command writes"),
        "writer refusal must say the command writes: {writes}"
    );
    assert!(
        writes.contains("read-only commands are admitted when the store schema matches"),
        "{writes}"
    );
    drop(joined);
    drop(old);
}

#[test]
fn read_only_join_refuses_when_store_schema_differs() {
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("record OLD"));
    let refusal = match read_only_join(root.path(), NEW, 22, || Ok(21)) {
        Ok(_) => panic!("schema mismatch must refuse a foreign read-only join"),
        Err(error) => error.to_string(),
    };
    assert!(
        refusal.contains("store schema 21 differs from compiled schema 22"),
        "{refusal}"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
}

#[test]
fn read_only_join_pins_first_generation_when_store_is_unavailable() {
    let root = tempfile::tempdir().expect("root");
    let joined = read_only_join(root.path(), NEW, 22, || {
        Err(crate::OrbitError::Execution("store is unavailable".into()))
    })
    .expect("a fresh root without a readable store uses the ordinary first pin");

    assert!(!joined.joined_foreign_generation());
    assert_eq!(
        std::fs::read_to_string(root.path().join(".generation.lock")).expect("record"),
        format!("1:{NEW}\n")
    );
}

#[test]
fn matching_digest_read_only_join_does_not_consult_store_schema() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let root = tempfile::tempdir().expect("root");
    drop(GenerationGuard::acquire(root.path(), OLD).expect("record OLD"));
    let probed = AtomicBool::new(false);
    let joined = read_only_join(root.path(), OLD, 22, || {
        probed.store(true, Ordering::SeqCst);
        Ok(99)
    })
    .expect("same-digest read-only join");
    assert!(!joined.joined_foreign_generation());
    assert!(
        !probed.load(Ordering::SeqCst),
        "matching digest must not require a schema probe"
    );
}

// APFS rejects directory names containing invalid UTF-8 with EILSEQ, so these
// byte-preservation fixtures are meaningful only on Unix filesystems that
// accept arbitrary path bytes.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_missing_root_creates_lock_files_under_exact_byte_path() {
    use std::os::unix::ffi::OsStrExt;
    let parent = tempfile::tempdir().expect("parent");
    let invalid_bytes = b"non_utf8_\xff_dir";
    let non_utf8_parent = parent
        .path()
        .join(std::ffi::OsStr::from_bytes(invalid_bytes));
    let root = non_utf8_parent.join("sub");
    drop(GenerationGuard::acquire(&root, OLD).expect("pin non-utf8 missing root"));
    assert!(root.is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
    assert!(root.join(".generation-admission.lock").exists());

    let entries: Vec<_> = std::fs::read_dir(parent.path())
        .expect("read parent")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].as_bytes(), invalid_bytes);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn distinct_non_utf8_roots_do_not_share_authority() {
    use std::os::unix::ffi::OsStrExt;
    let parent = tempfile::tempdir().expect("parent");
    let root1 = parent
        .path()
        .join(std::ffi::OsStr::from_bytes(b"non_utf8_\xff_dir"))
        .join("sub");
    let root2 = parent
        .path()
        .join(std::ffi::OsStr::from_bytes(b"non_utf8_\xfe_dir"))
        .join("sub");

    let guard1 = GenerationGuard::acquire(&root1, OLD).expect("pin root1");
    let guard2 = GenerationGuard::acquire(&root2, NEW).expect("pin root2 independently");

    assert!(root1.is_dir());
    assert!(root2.is_dir());
    assert_ne!(root1, root2);
    assert_eq!(
        std::fs::read_to_string(root1.join(".generation.lock")).expect("record root1"),
        format!("1:{OLD}\n")
    );
    assert_eq!(
        std::fs::read_to_string(root2.join(".generation.lock")).expect("record root2"),
        format!("1:{NEW}\n")
    );
    drop(guard1);
    drop(guard2);
}

#[test]
fn missing_root_with_double_dot_component_is_accepted_consistently() {
    let parent = tempfile::tempdir().expect("parent");
    let missing_root = parent.path().join("..data").join("sub");
    drop(GenerationGuard::acquire(&missing_root, OLD).expect("pin missing root with ..data"));
    assert!(missing_root.is_dir());
    assert_eq!(
        std::fs::read_to_string(missing_root.join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n")
    );
    assert!(missing_root.join(".generation-admission.lock").exists());

    let missing_root2 = parent.path().join("orbit..v2").join("sub");
    drop(GenerationGuard::acquire(&missing_root2, OLD).expect("pin missing root with orbit..v2"));
    assert!(missing_root2.is_dir());

    let parent2 = tempfile::tempdir().expect("parent2");
    let existing_parent = parent2.path().join("..data");
    std::fs::create_dir(&existing_parent).expect("create existing parent");
    let existing_parent_root = existing_parent.join("sub");
    drop(
        GenerationGuard::acquire(&existing_parent_root, OLD)
            .expect("pin existing parent root with ..data"),
    );
    assert!(existing_parent_root.is_dir());
}

#[test]
fn missing_root_escaping_start_with_parent_dir_component_is_refused() {
    let parent = tempfile::tempdir().expect("parent");
    let escaping_root = parent
        .path()
        .join("missing")
        .join("..")
        .join("..")
        .join("..")
        .join("escape");
    let error = match GenerationGuard::acquire(&escaping_root, OLD) {
        Ok(_) => panic!("an escaping root should be refused"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("generation root escapes its start")
            || error.contains("upgrade admission refused"),
        "{error}"
    );
    assert!(!parent.path().join(".generation.lock").exists());
    assert!(!parent.path().join(".generation-admission.lock").exists());
}

#[test]
fn builds_with_one_compatibility_share_a_generation_whatever_their_digest() {
    let root = tempfile::tempdir().expect("root");
    let current = identity(32, 28, 26);
    let first = join_writer(root.path(), OLD, &current).expect("first writer");
    let rebuilt = join_writer(root.path(), NEW, &current).expect("same-compatibility writer");
    assert!(!rebuilt.joined_foreign_generation());
    let reader = join_as(
        root.path(),
        THIRD,
        &current,
        ParticipantRole::Dashboard,
        Access::ReadOnly,
        Duration::ZERO,
    )
    .expect("same-compatibility reader");
    assert!(reader.joined_foreign_generation());
    assert_eq!(
        std::fs::read_to_string(root.path().join(".generation.lock")).expect("record"),
        format!("1:{OLD}\n"),
        "joining must not rewrite the executable-generation-v1 record"
    );
    assert!(GenerationUpdate::acquire(root.path()).is_err());
    drop((first, rebuilt, reader));
    assert!(GenerationUpdate::acquire(root.path()).is_ok());
}

#[test]
fn an_additive_upgrade_joins_live_writers_and_older_writers_keep_joining() {
    let root = tempfile::tempdir().expect("root");
    let older = identity(32, 28, 26);
    let newer = identity(33, 28, 26);
    let live = join_writer(root.path(), OLD, &older).expect("older writer");
    let upgraded = join_writer(root.path(), NEW, &newer).expect("additive upgrade joins");
    let late_older = join_writer(root.path(), OLD, &older)
        .expect("an older writer still joins after an additive upgrade");
    drop((live, upgraded, late_older));
}

#[test]
fn a_breaking_upgrade_names_every_blocker_when_the_bound_expires() {
    let root = tempfile::tempdir().expect("root");
    let older = identity(32, 28, 26);
    let breaking = identity(33, 33, 26);
    let _drain = join_as(
        root.path(),
        OLD,
        &older,
        ParticipantRole::Drain,
        Access::Write,
        Duration::ZERO,
    )
    .expect("older drain");
    let _mcp = join_as(
        root.path(),
        OLD,
        &older,
        ParticipantRole::McpServe,
        Access::Write,
        Duration::ZERO,
    )
    .expect("older mcp serve");
    let refusal = refusal_text(join_writer(root.path(), NEW, &breaking));
    let pid = std::process::id();
    assert!(refusal.contains("did not yield within 0s"), "{refusal}");
    assert!(
        refusal.contains(&format!("pid {pid} (drain, started ")),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("pid {pid} (mcp serve, started ")),
        "{refusal}"
    );
    assert!(
        pending_switch(root.path()).is_none(),
        "an expired wait must clear its pending switch"
    );
    join_writer(root.path(), OLD, &older).expect("the live generation still admits its own");
}

#[test]
fn a_pending_breaking_upgrade_refuses_newcomers_and_waits_for_live_participants() {
    let root = tempfile::tempdir().expect("root");
    let older = identity(32, 28, 26);
    let breaking = identity(33, 33, 26);
    let live = join_writer(root.path(), OLD, &older).expect("older writer");
    let waiter = {
        let root = root.path().to_path_buf();
        let breaking = breaking.clone();
        std::thread::spawn(move || join_writer_with(&root, NEW, &breaking, Duration::from_secs(20)))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let switch = loop {
        if let Some(switch) = pending_switch(root.path()) {
            break switch;
        }
        assert!(
            Instant::now() < deadline,
            "the upgrade never recorded its pending switch"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(switch.target, breaking);
    assert_eq!(switch.digest, NEW);

    // No participant the switch would have to wait for is admitted meanwhile,
    // and a clock tick treats it as a hold rather than a failure.
    let error = join_writer(root.path(), OLD, &older)
        .err()
        .expect("old newcomer refused");
    assert!(error.to_string().contains("switch is pending"), "{error}");
    assert!(is_clock_generation_hold(&error));
    assert!(GenerationUpdate::acquire(root.path()).is_err());

    drop(live);
    let upgraded = waiter
        .join()
        .expect("waiter thread")
        .expect("the upgrade is admitted once the live writer exits");
    assert!(pending_switch(root.path()).is_none());
    assert_eq!(
        std::fs::read_to_string(root.path().join(".generation.lock")).expect("record"),
        format!("1:{NEW}\n")
    );
    let refusal = refusal_text(join_writer(root.path(), OLD, &older));
    assert!(refusal.contains("incompatible"), "{refusal}");
    join_writer(root.path(), THIRD, &breaking).expect("the new compatibility joins");
    drop(upgraded);
}

fn join_writer_with(
    root: &Path,
    digest: &str,
    identity: &CompatibilityIdentity,
    quiesce: Duration,
) -> Result<GenerationGuard, crate::OrbitError> {
    join_as(
        root,
        digest,
        identity,
        ParticipantRole::Command,
        Access::Write,
        quiesce,
    )
}

#[test]
fn an_older_binary_never_displaces_a_newer_generation() {
    let root = tempfile::tempdir().expect("root");
    let newer = identity(33, 33, 26);
    let older = identity(32, 28, 26);
    let _live = join_writer(root.path(), NEW, &newer).expect("newer writer");
    let started = Instant::now();
    let refusal = refusal_text(join_writer_with(
        root.path(),
        OLD,
        &older,
        Duration::from_secs(30),
    ));
    assert!(refusal.contains("incompatible"), "{refusal}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an older binary must be refused, not wait"
    );
    assert!(pending_switch(root.path()).is_none());
}

#[test]
fn executable_generation_v1_holders_refuse_v2_writers_at_once() {
    let root = tempfile::tempdir().expect("root");
    let current = identity(32, 28, 26);
    let v1 = GenerationGuard::acquire(root.path(), OLD).expect("v1 holder");
    let started = Instant::now();
    let refusal = refusal_text(join_writer_with(
        root.path(),
        NEW,
        &current,
        Duration::from_secs(30),
    ));
    assert!(refusal.contains("this command writes"), "{refusal}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "v1 holders never yield, so waiting for them would only stall"
    );
    let reader = read_only_join(root.path(), NEW, 32, || Ok(32))
        .expect("a same-schema v2 reader joins a v1 generation");
    assert!(reader.joined_foreign_generation());
    drop((v1, reader));
}

#[test]
fn v2_participants_refuse_v1_writers_and_v1_takeovers_invalidate_the_envelope() {
    let root = tempfile::tempdir().expect("root");
    let current = identity(32, 28, 26);
    let v2 = join_writer(root.path(), NEW, &current).expect("v2 writer");
    // A v1 binary sees only `.generation.lock`, which names the v2 digest.
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    drop(v2);

    // Quiescent: the v1 binary takes over and rewrites the record, so the
    // v2 envelope no longer describes the live generation.
    let v1 = GenerationGuard::acquire(root.path(), OLD).expect("v1 takeover");
    let refusal = refusal_text(join_writer(root.path(), THIRD, &current));
    assert!(refusal.contains("this command writes"), "{refusal}");
    drop(v1);
    join_writer(root.path(), THIRD, &current).expect("v2 takeover once v1 exits");
}

#[test]
fn participants_register_while_live_and_withdraw_on_exit() {
    let root = tempfile::tempdir().expect("root");
    let current = identity(32, 28, 26);
    let registrations = || {
        std::fs::read_dir(root.path().join(".generation-participants"))
            .map(|entries| entries.count())
            .unwrap_or(0)
    };
    let first = join_writer(root.path(), OLD, &current).expect("first");
    let second = join_writer(root.path(), NEW, &current).expect("second");
    assert_eq!(registrations(), 2);
    drop(first);
    assert_eq!(registrations(), 1);
    drop(second);
    assert_eq!(registrations(), 0);
}

#[test]
fn an_update_pin_with_an_identity_admits_compatible_builds() {
    let root = tempfile::tempdir().expect("root");
    let current = identity(32, 28, 26);
    let candidate = GenerationUpdate::acquire(root.path())
        .expect("quiescent")
        .pin(NEW, Some(&current))
        .expect("pin candidate");
    join_writer(root.path(), THIRD, &current)
        .expect("a compatible build joins the candidate's generation");
    drop(candidate);

    let v1_candidate = GenerationUpdate::acquire(root.path())
        .expect("quiescent")
        .pin(OLD, None)
        .expect("pin a v1 candidate");
    let refusal = refusal_text(join_writer(root.path(), THIRD, &current));
    assert!(refusal.contains("this command writes"), "{refusal}");
    drop(v1_candidate);
}
