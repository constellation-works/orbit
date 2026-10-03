//! Forge fixtures for the host-only recovery authority.
//!
//! Every attack here models the same capability the sandbox actually grants a
//! managed leaf: arbitrary bytes in the shared run store. The certificate the
//! host wrote lives elsewhere, so the fixture edits the *checkpoint* and
//! asserts the record no longer matches.

use std::path::Path;

use orbit_types::policy::ResolvedFsProfile;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{RecoveryAuthority, append_recovery_authority_denies};

const RUN_ID: &str = "jrun-20260910-0001-1";
const OTHER_RUN_ID: &str = "jrun-20260910-0002-1";
const STEP_ID: &str = "sync_base";

fn checkpoint(run_id: &str, step_id: &str, workspace: &Path) -> Value {
    json!({
        "run_id": run_id,
        "step_id": step_id,
        "task_ids": ["ORB-11977"],
        "workspace_path": workspace,
        "head": "orbit/ORB-11977",
        "head_sha_before": "1111111111111111111111111111111111111111",
        "original_base_sha": "2222222222222222222222222222222222222222",
        "base_ref": "refs/remotes/origin/agent-main",
        "base_sha": "3333333333333333333333333333333333333333",
        "remote_sha_before": Value::Null,
        "head_sha": "4444444444444444444444444444444444444444",
        "rewritten": true,
    })
}

fn fixture() -> (TempDir, TempDir, Value) {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    let accepted = checkpoint(RUN_ID, STEP_ID, workspace.path());
    let authority = RecoveryAuthority::open(global.path()).expect("open authority");
    authority
        .issue(RUN_ID, STEP_ID, &accepted)
        .expect("issue certificate");
    (global, workspace, accepted)
}

#[test]
fn certified_evidence_verifies_and_every_leaf_edit_does_not() {
    let (global, workspace, accepted) = fixture();
    let authority = RecoveryAuthority::open(global.path()).expect("reopen authority");

    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &accepted)
            .expect("verify accepted"),
        "the host's own evidence must verify",
    );

    // A leaf owns the run row, so it can change any field in it. Each edit
    // below is a distinct forgery shape and each must be rejected.
    for (label, forged) in [
        ("rewritten head", {
            let mut forged = accepted.clone();
            forged["head_sha"] = json!("5555555555555555555555555555555555555555");
            forged
        }),
        ("pinned base", {
            let mut forged = accepted.clone();
            forged["base_sha"] = json!("6666666666666666666666666666666666666666");
            forged
        }),
        ("owning tasks", {
            let mut forged = accepted.clone();
            forged["task_ids"] = json!(["ORB-00000"]);
            forged
        }),
        ("rewritten flag", {
            let mut forged = accepted.clone();
            forged["rewritten"] = json!(false);
            forged
        }),
        ("added field", {
            let mut forged = accepted.clone();
            forged["injected"] = json!("leaf");
            forged
        }),
        ("removed field", {
            let mut forged = accepted.clone();
            forged
                .as_object_mut()
                .expect("object payload")
                .remove("remote_sha_before");
            forged
        }),
    ] {
        assert!(
            !authority
                .verify(RUN_ID, STEP_ID, &forged)
                .expect("verify forged"),
            "a forged `{label}` must not verify",
        );
    }

    // Cross-run and cross-step substitution: the payload names its own run and
    // step, so a copy into another run's row disagrees with the record.
    let other_workspace = TempDir::new().expect("other workspace");
    for (label, run_id, step_id, forged) in [
        (
            "another run",
            OTHER_RUN_ID,
            STEP_ID,
            checkpoint(OTHER_RUN_ID, STEP_ID, workspace.path()),
        ),
        (
            "another step",
            RUN_ID,
            "complete_pr",
            checkpoint(RUN_ID, "complete_pr", workspace.path()),
        ),
        (
            "another workspace",
            RUN_ID,
            STEP_ID,
            checkpoint(RUN_ID, STEP_ID, other_workspace.path()),
        ),
    ] {
        assert!(
            !authority
                .verify(run_id, step_id, &forged)
                .expect("verify substituted"),
            "evidence rebound to `{label}` must not verify",
        );
    }

    // A leaf may also leave the payload alone and relabel where it is stored.
    // Reading the record under the relabelled identity finds nothing.
    assert!(
        !authority
            .verify(OTHER_RUN_ID, STEP_ID, &accepted)
            .expect("verify relabelled run"),
        "the accepted payload must not verify under another run's identity",
    );
    assert!(
        !authority
            .verify(RUN_ID, "complete_pr", &accepted)
            .expect("verify relabelled step"),
        "the accepted payload must not verify under another step's identity",
    );
}

#[test]
fn an_accepted_certificate_cannot_be_replaced() {
    let (global, _workspace, accepted) = fixture();
    let authority = RecoveryAuthority::open(global.path()).expect("reopen authority");

    // Re-issuing identical evidence is a harmless retry.
    authority
        .issue(RUN_ID, STEP_ID, &accepted)
        .expect("idempotent reissue");

    let mut replacement = accepted.clone();
    replacement["head_sha"] = json!("7777777777777777777777777777777777777777");
    let error = authority
        .issue(RUN_ID, STEP_ID, &replacement)
        .expect_err("replacing accepted evidence must fail");
    assert!(error.to_string().contains("immutable"), "{error}");

    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &accepted)
            .expect("verify original"),
        "the original certificate survives the refused replacement",
    );
    assert!(
        !authority
            .verify(RUN_ID, STEP_ID, &replacement)
            .expect("verify replacement"),
    );
}

/// The regression this module exists for. A symlink standing in for a component
/// *below* the trusted root used to be followed by `create_dir_all` and then
/// declared clean, because the symlink check ran on an already canonicalized
/// path. Each layout below must be refused with nothing created at the
/// redirection target.
#[cfg(unix)]
#[test]
fn a_symlink_below_the_trusted_root_is_refused_before_anything_is_created() {
    use std::os::unix::fs::symlink;

    for (label, link, target_probe) in [
        ("authority parent", "state", "recovery-authority"),
        ("authority root", "state/recovery-authority", "authority.db"),
    ] {
        let global = TempDir::new().expect("global root");
        let elsewhere = TempDir::new().expect("redirection target");
        let link = global.path().join(link);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).expect("link parent");
        }
        symlink(elsewhere.path(), &link).expect("plant redirection symlink");

        let error =
            RecoveryAuthority::open(global.path()).expect_err("a redirected component must fail");
        assert!(
            error.to_string().contains("symlinked path"),
            "`{label}` must be refused as a symlink: {error}",
        );
        assert!(
            !elsewhere.path().join(target_probe).exists(),
            "`{label}` redirected authority state into `{}`",
            elsewhere.path().display(),
        );
        assert!(
            link.symlink_metadata()
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the planted `{label}` link must be left untouched, not written through",
        );

        // The deny appended to a sandbox profile derives from the same root, so
        // it must refuse the redirected layout too rather than name a path the
        // authority never uses.
        let mut resolved = ResolvedFsProfile {
            name: "implementer".to_string(),
            read: vec!["/**".to_string()],
            modify: Vec::new(),
        };
        let error = append_recovery_authority_denies(global.path(), &mut resolved)
            .expect_err("a redirected root must not yield a deny rule");
        assert!(error.to_string().contains("symlinked path"), "{error}");
        assert!(resolved.modify.is_empty());
    }
}

/// A symlinked database file keeps every directory on the way there looking
/// correct while the certificate is read from, and written to, a file outside
/// the protected root.
#[cfg(unix)]
#[test]
fn a_symlinked_authority_database_is_refused() {
    use std::os::unix::fs::symlink;

    for name in ["authority.db", "authority.db-wal", "authority.db-shm"] {
        let (global, _workspace, _accepted) = fixture();
        let elsewhere = TempDir::new().expect("redirection target");
        let planted = elsewhere.path().join("planted.db");
        std::fs::write(&planted, b"planted").expect("planted file");

        let file = global.path().join("state/recovery-authority").join(name);
        std::fs::remove_file(&file).ok();
        symlink(&planted, &file).expect("plant database symlink");

        let error = RecoveryAuthority::open(global.path())
            .expect_err("a symlinked database file must not be opened");
        assert!(
            error.to_string().contains("symlinked path"),
            "`{name}` must be refused as a symlink: {error}",
        );
        assert_eq!(
            std::fs::read(&planted).expect("planted contents"),
            b"planted",
            "`{name}` let the authority write outside the protected root",
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn worker_process_binding_survives_descendants_and_forged_environment() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    if let Some(root) = std::env::var_os("ORBIT_BINDING_FIXTURE_ROOT") {
        if std::env::var_os("ORBIT_BINDING_GRANDCHILD").is_none() {
            std::io::stdin()
                .read_exact(&mut [0u8; 1])
                .expect("parent binding barrier");
        }
        let binding = match std::env::var_os("ORBIT_BINDING_PROC_ROOT") {
            Some(proc_root) => {
                super::current_worker_binding_in(Path::new(&root), Path::new(&proc_root))
            }
            None => super::current_worker_binding(Path::new(&root)),
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
            let output = command.args(["--exact", "runtime::recovery_authority::tests::worker_process_binding_survives_descendants_and_forged_environment", "--nocapture"])
                .env("ORBIT_BINDING_FIXTURE_ROOT", root).env("ORBIT_BINDING_GRANDCHILD", "1")
                .env("ORBIT_RUN_ID", "forged-grandchild-run").output().expect("grandchild");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
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
    let mut child = command.args(["--exact", "runtime::recovery_authority::tests::worker_process_binding_survives_descendants_and_forged_environment", "--nocapture"])
        .env("HOME", root.path()).env("USERPROFILE", root.path())
        .env("ORBIT_BINDING_FIXTURE_ROOT", root.path()).env("ORBIT_RUN_ID", "forged-run")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("child");
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
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        super::current_worker_binding(root.path())
            .expect("no matching live process")
            .is_none()
    );
    authority
        .record_worker_namespace("different-namespace", &binding)
        .expect("unrelated namespace");
    assert!(
        super::current_worker_binding(root.path())
            .expect("unrelated namespace refused")
            .is_none()
    );
    // A synthetic namespace leader keeps these assertions runnable wherever
    // `/proc/1/ns/pid` is not readable for the caller -- a CI runner is not
    // PID 1's namespace peer, and the resolver now reads that denial as "no
    // binding" rather than refusing to open.
    let proc_root = synthetic_proc_root();
    let namespace = super::namespace_key(proc_root.path(), 1).expect("synthetic init identity");
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
    let output = retry.args(["--exact", "runtime::recovery_authority::tests::worker_process_binding_survives_descendants_and_forged_environment", "--nocapture"])
        .env("HOME", root.path()).env("USERPROFILE", root.path()).env("ORBIT_BINDING_FIXTURE_ROOT", root.path())
        .env("ORBIT_BINDING_PROC_ROOT", proc_root.path())
        .env("ORBIT_BINDING_GRANDCHILD", "1").env("ORBIT_RUN_ID", "forged-retry")
        .output().expect("new process with namespace binding");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(unix)]
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
