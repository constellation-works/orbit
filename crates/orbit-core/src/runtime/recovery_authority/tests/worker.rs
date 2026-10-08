use super::super::{RecoveryAuthority, worker};
use std::path::Path;
use tempfile::TempDir;

#[cfg(target_os = "linux")]
#[test]
fn worker_process_binding_survives_descendants_and_forged_environment() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    const CHILD_TEST: &str = "runtime::recovery_authority::tests::worker::worker_process_binding_survives_descendants_and_forged_environment";
    if let Some(root) = std::env::var_os("ORBIT_BINDING_FIXTURE_ROOT") {
        if std::env::var_os("ORBIT_BINDING_GRANDCHILD").is_none() {
            std::io::stdin()
                .read_exact(&mut [0u8; 1])
                .expect("parent binding barrier");
        }
        let binding = match std::env::var_os("ORBIT_BINDING_PROC_ROOT") {
            Some(proc_root) => {
                worker::current_worker_binding_in(Path::new(&root), Path::new(&proc_root))
            }
            None => worker::current_worker_binding(Path::new(&root)),
        }
        .expect("resolve authority")
        .expect("bound ancestor");
        assert_eq!(binding.bound_run_id, "immutable-leaf");
        assert_eq!(binding.execution.machine_id, "execution-machine");
        assert_ne!(
            binding.bound_run_id,
            std::env::var("ORBIT_RUN_ID").expect("forged env")
        );
        if std::env::var_os("ORBIT_BINDING_GRANDCHILD").is_none() {
            let mut command = Command::new(std::env::current_exe().expect("test binary"));
            orbit_common::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            let output = command
                .args(["--exact", CHILD_TEST, "--nocapture"])
                .env("ORBIT_BINDING_FIXTURE_ROOT", root)
                .env("ORBIT_BINDING_GRANDCHILD", "1")
                .env("ORBIT_RUN_ID", "forged-grandchild-run")
                .output()
                .expect("grandchild");
            orbit_common::test_env::assert_child_test_passed(
                CHILD_TEST,
                output.status,
                &output.stdout,
                &output.stderr,
            );
        }
        return;
    }
    let root = TempDir::new().expect("authority root");
    let authority = RecoveryAuthority::open(root.path()).expect("authority");
    let binding = worker_binding();
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let mut child = command
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("ORBIT_BINDING_FIXTURE_ROOT", root.path())
        .env("ORBIT_RUN_ID", "forged-run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("child");
    authority
        .bind_worker_process(child.id(), &binding)
        .expect("bind");
    authority
        .bind_worker_process(child.id(), &binding)
        .expect("same process retry");
    let mut forged = binding.clone();
    forged.bound_run_id = "replacement".into();
    assert!(authority.bind_worker_process(child.id(), &forged).is_err());
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"1")
        .expect("release child");
    let output = child.wait_with_output().expect("child output");
    orbit_common::test_env::assert_child_test_passed(
        CHILD_TEST,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    assert!(
        worker::current_worker_binding(root.path())
            .expect("no matching live process")
            .is_none()
    );
    authority
        .record_worker_namespace("different-namespace", &binding)
        .expect("unrelated namespace");
    assert!(
        worker::current_worker_binding(root.path())
            .expect("unrelated namespace refused")
            .is_none()
    );
    // A synthetic namespace leader keeps these assertions runnable wherever
    // `/proc/1/ns/pid` is not readable for the caller -- a CI runner is not
    // PID 1's namespace peer, and the resolver now reads that denial as "no
    // binding" rather than refusing to open.
    let proc_root = synthetic_proc_root();
    let namespace = worker::namespace_key(proc_root.path(), 1).expect("synthetic init identity");
    authority
        .record_worker_namespace(&namespace, &binding)
        .expect("seed namespace identity");
    authority
        .record_worker_namespace(&namespace, &binding)
        .expect("namespace retry");
    assert!(
        authority
            .record_worker_namespace(&namespace, &forged)
            .is_err()
    );
    let mut retry = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        retry.env_remove(key);
    });
    let output = retry
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("ORBIT_BINDING_FIXTURE_ROOT", root.path())
        .env("ORBIT_BINDING_PROC_ROOT", proc_root.path())
        .env("ORBIT_BINDING_GRANDCHILD", "1")
        .env("ORBIT_RUN_ID", "forged-retry")
        .output()
        .expect("new process with namespace binding");
    orbit_common::test_env::assert_child_test_passed(
        CHILD_TEST,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

#[cfg(target_os = "linux")]
fn worker_binding() -> orbit_types::tool::WorkerInvocation {
    orbit_types::tool::WorkerInvocation {
        owner_machine_id: "owner-machine".into(),
        owner_workspace_id: "workspace".into(),
        owner_destination: "owner/workspace".into(),
        task_id: "task".into(),
        claim_id: "claim".into(),
        execution: orbit_types::task::ExecutionLocation {
            machine_id: "execution-machine".into(),
            machine_name: None,
        },
        bound_run_id: "immutable-leaf".into(),
    }
}

/// A `/proc` stand-in whose PID 1 the caller owns: the namespace link, the
/// start identity in field 22 of `stat`, and the boot id the key pins.
#[cfg(target_os = "linux")]
fn synthetic_proc_root() -> TempDir {
    let root = TempDir::new().expect("proc root");
    let leader = root.path().join("1");
    std::fs::create_dir_all(leader.join("ns")).expect("leader ns");
    std::os::unix::fs::symlink("pid:[4026531836]", leader.join("ns/pid")).expect("namespace link");
    std::fs::write(
        leader.join("stat"),
        format!("1 (systemd) S{} 8241\n", " 0".repeat(18)),
    )
    .expect("leader stat");
    let random = root.path().join("sys/kernel/random");
    std::fs::create_dir_all(&random).expect("boot id dir");
    std::fs::write(
        random.join("boot_id"),
        "d5a1f0c2-3b4e-4f5a-8c6d-7e8f90a1b2c3\n",
    )
    .expect("boot id");
    root
}
