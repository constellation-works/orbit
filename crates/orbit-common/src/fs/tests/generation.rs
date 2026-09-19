use crate::fs::generation::{GenerationGuard, GenerationUpdate};

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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
        .pin(NEW)
        .expect("pin candidate");
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    let convergence = GenerationGuard::acquire(root.path(), NEW).expect("candidate child");
    drop(candidate);
    assert!(GenerationGuard::acquire(root.path(), OLD).is_err());
    drop(convergence);
    assert!(GenerationUpdate::acquire(root.path()).is_ok());
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
    assert!(GenerationGuard::acquire(root.path(), OLD).is_ok());
}

#[test]
fn interrupted_exclusive_holder_releases_os_locks_without_repair() {
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

    // Observation still resolves: `update --preflight` reports admission
    // without reserving it, and never writes.
    let update = GenerationUpdate::acquire(root.path()).expect("read-only admission observation");
    let refusal = refusal_of(update.pin(NEW));
    assert!(refusal.contains("cannot be written from here"), "{refusal}");
    assert_eq!(std::fs::read(&record).expect("record"), before);
    thaw_records(root.path());
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
