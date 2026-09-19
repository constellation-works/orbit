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
