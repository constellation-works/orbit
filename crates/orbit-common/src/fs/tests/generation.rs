use crate::fs::generation::{GenerationGuard, GenerationUpdate};

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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
    let refusal = refusal_of(update.pin(NEW));
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
