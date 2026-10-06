//! Forge fixtures for the host-only recovery authority.
//!
//! Every attack here models the same capability the sandbox actually grants a
//! managed leaf: arbitrary bytes in the shared run store. The certificate the
//! host wrote lives elsewhere, so the fixture edits the *checkpoint* and
//! asserts the record no longer matches.

use std::path::Path;

use orbit_engine::RebaseRecoveryAttemptScope;
use orbit_types::policy::ResolvedFsProfile;
use rusqlite::params;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{RecoveryAuthority, append_recovery_authority_denies, payload_digest};

const RUN_ID: &str = "jrun-20260910-0001-1";
const OTHER_RUN_ID: &str = "jrun-20260910-0002-1";
const STEP_ID: &str = "sync_base";
const HEAD_BEFORE: &str = "1111111111111111111111111111111111111111";
const PINNED_BASE: &str = "3333333333333333333333333333333333333333";

fn checkpoint(run_id: &str, step_id: &str, workspace: &Path) -> Value {
    json!({
        "run_id": run_id,
        "step_id": step_id,
        "task_ids": ["ORB-11977"],
        "workspace_path": workspace,
        "head": "orbit/ORB-11977",
        "head_sha_before": HEAD_BEFORE,
        "original_base_sha": "2222222222222222222222222222222222222222",
        "base_ref": "refs/remotes/origin/agent-main",
        "target_base_sha": PINNED_BASE,
        "base_sha": PINNED_BASE,
        "remote_sha_before": Value::Null,
        "head_sha": "4444444444444444444444444444444444444444",
        "rewritten": true,
        "recovery_attempt": 1,
    })
}

fn scope(workspace: &Path) -> RebaseRecoveryAttemptScope {
    RebaseRecoveryAttemptScope {
        workspace_path: workspace.to_string_lossy().into_owned(),
        head_sha_before: HEAD_BEFORE.to_string(),
        target_base_sha: PINNED_BASE.to_string(),
    }
}

/// `previous`, completed again by a later attempt that landed another HEAD.
fn later_attempt(previous: &Value, attempt: u64, head_sha: &str) -> Value {
    let mut later = previous.clone();
    later["recovery_attempt"] = json!(attempt);
    later["head_sha"] = json!(head_sha);
    later
}

fn fixture() -> (TempDir, TempDir, Value) {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    let accepted = checkpoint(RUN_ID, STEP_ID, workspace.path());
    let authority = RecoveryAuthority::open(global.path()).expect("open authority");
    let attempt = authority
        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
        .expect("reserve attempt");
    assert_eq!(attempt, 1);
    authority
        .issue(RUN_ID, STEP_ID, &accepted)
        .expect("issue certificate");
    (global, workspace, accepted)
}

/// Every attempt certificate row, as `(run, step, attempt, digest, issued_at)`.
fn certificate_rows(authority: &RecoveryAuthority) -> Vec<(String, String, i64, String, String)> {
    let mut statement = authority
        .connection
        .prepare(
            "SELECT run_id, step_id, attempt, payload_digest, issued_at
             FROM rebase_recovery_attempt_certificate ORDER BY run_id, step_id, attempt",
        )
        .expect("prepare certificate scan");
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("scan certificates")
        .collect::<Result<_, _>>()
        .expect("read certificates")
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
        ("selected attempt", {
            let mut forged = accepted.clone();
            forged["recovery_attempt"] = json!(2);
            forged
        }),
        ("attempt removed to look pre-attempt", {
            let mut forged = accepted.clone();
            forged
                .as_object_mut()
                .expect("object payload")
                .remove("recovery_attempt");
            forged
        }),
        ("malformed attempt", {
            let mut forged = accepted.clone();
            forged["recovery_attempt"] = json!("1");
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

/// [F2026-10-041] Recovery A certified `sync_base`; the run resumed, the step
/// conflicted again, and recovery B of the same step was refused as a
/// replacement of A. B is a new host-reserved attempt: it certifies beside A,
/// A's record is never rewritten, and only B vouches for the step afterwards.
#[test]
fn a_later_recovery_attempt_certifies_beside_the_earlier_one() {
    let (global, workspace, recovery_a) = fixture();
    let authority = RecoveryAuthority::open(global.path()).expect("reopen authority");
    let before = certificate_rows(&authority);

    let attempt = authority
        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
        .expect("reserve recovery B");
    assert_eq!(attempt, 2, "the host assigns the next attempt");
    let recovery_b = later_attempt(
        &recovery_a,
        attempt,
        "8888888888888888888888888888888888888888",
    );

    // Recovery A cannot be re-certified once B is admitted, even unchanged.
    let error = authority
        .issue(RUN_ID, STEP_ID, &recovery_a)
        .expect_err("a superseded attempt must not certify");
    assert!(error.to_string().contains("superseded"), "{error}");

    authority
        .issue(RUN_ID, STEP_ID, &recovery_b)
        .expect("recovery B certifies as its own attempt");
    authority
        .issue(RUN_ID, STEP_ID, &recovery_b)
        .expect("recovery B's identical retry is idempotent");

    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &recovery_b)
            .expect("verify B")
    );
    assert!(
        !authority
            .verify(RUN_ID, STEP_ID, &recovery_a)
            .expect("verify A"),
        "a replayed earlier attempt must not vouch for the step once B is certified",
    );
    let after = certificate_rows(&authority);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], before[0], "A's certificate is never rewritten");
    assert_eq!(after[1].2, 2);
    assert_eq!(after[1].3, payload_digest(&recovery_b));

    // B's attempt cannot carry other evidence, nor can an unreserved attempt,
    // nor can B be rebound to another checkout, original HEAD or pinned base.
    let other_workspace = TempDir::new().expect("other workspace");
    for (label, forged, refusal) in [
        (
            "changed evidence",
            later_attempt(&recovery_b, 2, "9999999999999999999999999999999999999999"),
            "immutable",
        ),
        (
            "unreserved attempt",
            later_attempt(&recovery_b, 3, "9999999999999999999999999999999999999999"),
            "not reserved",
        ),
        (
            "another workspace",
            {
                let mut forged = recovery_b.clone();
                forged["workspace_path"] = json!(other_workspace.path());
                forged
            },
            "reserved workspace",
        ),
        (
            "another original HEAD",
            {
                let mut forged = recovery_b.clone();
                forged["head_sha_before"] = json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                forged
            },
            "reserved workspace",
        ),
        (
            "another pinned base",
            {
                let mut forged = recovery_b.clone();
                forged["target_base_sha"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
                forged
            },
            "reserved workspace",
        ),
        (
            "no attempt",
            {
                let mut forged = recovery_b.clone();
                forged
                    .as_object_mut()
                    .expect("object payload")
                    .remove("recovery_attempt");
                forged
            },
            "no host-reserved attempt",
        ),
    ] {
        let error = authority
            .issue(RUN_ID, STEP_ID, &forged)
            .expect_err("forged issuance must fail");
        assert!(error.to_string().contains(refusal), "{label}: {error}");
    }
    // An attempt reserved for one run does not exist in another.
    let error = authority
        .issue(
            OTHER_RUN_ID,
            STEP_ID,
            &checkpoint(OTHER_RUN_ID, STEP_ID, workspace.path()),
        )
        .expect_err("another run's attempt was never reserved");
    assert!(error.to_string().contains("not reserved"), "{error}");
    assert_eq!(
        certificate_rows(&authority),
        after,
        "no refusal wrote a record"
    );
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &recovery_b)
            .expect("verify B")
    );
}

/// The pre-attempt shape: one `rebase_recovery_certificate` row per run and
/// step, written by an older binary for a payload without an attempt. It keeps
/// vouching for that payload until a newer attempt of the step is certified,
/// it is never written again, and older binaries keep reading the same table.
#[test]
fn a_pre_attempt_certificate_verifies_until_a_new_attempt_is_certified() {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    let mut legacy = checkpoint(RUN_ID, STEP_ID, workspace.path());
    legacy
        .as_object_mut()
        .expect("object payload")
        .remove("recovery_attempt");
    {
        // Lay the database out exactly as the previous binary did.
        let authority = RecoveryAuthority::open(global.path()).expect("open authority");
        authority
            .connection
            .execute_batch(
                "DROP TABLE rebase_recovery_attempt;
                 DROP TABLE rebase_recovery_attempt_certificate;",
            )
            .expect("strip attempt tables");
        authority
            .connection
            .execute(
                "INSERT INTO rebase_recovery_certificate
                     (run_id, step_id, workspace_path, head_sha, base_sha, payload_digest,
                      issued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, '2026-10-04T22:04:41Z')",
                params![
                    RUN_ID,
                    STEP_ID,
                    workspace.path().to_string_lossy(),
                    legacy["head_sha"].as_str(),
                    legacy["base_sha"].as_str(),
                    payload_digest(&legacy),
                ],
            )
            .expect("write legacy certificate");
    }

    let authority = RecoveryAuthority::open(global.path()).expect("open upgraded authority");
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &legacy)
            .expect("verify legacy"),
        "a run resumed across the upgrade keeps its certified recovery",
    );
    let error = authority
        .issue(RUN_ID, STEP_ID, &legacy)
        .expect_err("new code never issues the pre-attempt shape");
    assert!(
        error.to_string().contains("no host-reserved attempt"),
        "{error}"
    );
    // Old evidence relabelled with an attempt is not certified by anything.
    let mut relabelled = legacy.clone();
    relabelled["recovery_attempt"] = json!(1);
    assert!(
        !authority
            .verify(RUN_ID, STEP_ID, &relabelled)
            .expect("verify relabelled")
    );

    let attempt = authority
        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
        .expect("reserve the first attempt after the upgrade");
    // A reservation alone does not displace the certified legacy evidence.
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &legacy)
            .expect("verify legacy")
    );
    let recovery = later_attempt(&legacy, attempt, "8888888888888888888888888888888888888888");
    authority
        .issue(RUN_ID, STEP_ID, &recovery)
        .expect("the new attempt certifies beside the legacy record");
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &recovery)
            .expect("verify new")
    );
    assert!(
        !authority
            .verify(RUN_ID, STEP_ID, &legacy)
            .expect("verify legacy"),
        "old evidence must not outlive a newer certified attempt",
    );
    let legacy_digest: String = authority
        .connection
        .query_row(
            "SELECT payload_digest FROM rebase_recovery_certificate
             WHERE run_id = ?1 AND step_id = ?2",
            params![RUN_ID, STEP_ID],
            |row| row.get(0),
        )
        .expect("legacy row survives");
    assert_eq!(legacy_digest, payload_digest(&legacy));
}

/// Two host processes completing the same attempt race to issue it; two
/// admitted recoveries race to reserve. Every identical issuance succeeds with
/// one record, at most one of two different payloads wins, and reservations
/// never share a number.
#[test]
fn concurrent_issuance_and_reservation_stay_consistent() {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    RecoveryAuthority::open(global.path()).expect("create authority");

    let reserved = std::thread::scope(|threads| {
        let handles = (0..4)
            .map(|_| {
                threads.spawn(|| {
                    RecoveryAuthority::open(global.path())
                        .expect("open authority")
                        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
                        .expect("reserve attempt")
                })
            })
            .collect::<Vec<_>>();
        let mut reserved = handles
            .into_iter()
            .map(|handle| handle.join().expect("reservation thread"))
            .collect::<Vec<_>>();
        reserved.sort_unstable();
        reserved
    });
    assert_eq!(reserved, vec![1, 2, 3, 4]);

    let current = later_attempt(
        &checkpoint(RUN_ID, STEP_ID, workspace.path()),
        4,
        "4444444444444444444444444444444444444444",
    );
    let rival = later_attempt(&current, 4, "5555555555555555555555555555555555555555");
    let outcomes = std::thread::scope(|threads| {
        [&current, &current, &current, &rival]
            .into_iter()
            .map(|payload| {
                threads.spawn(|| {
                    RecoveryAuthority::open(global.path())
                        .expect("open authority")
                        .issue(RUN_ID, STEP_ID, payload)
                        .is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("issuance thread"))
            .collect::<Vec<_>>()
    });
    let authority = RecoveryAuthority::open(global.path()).expect("reopen authority");
    let rows = certificate_rows(&authority);
    assert_eq!(rows.len(), 1, "one attempt holds one certificate");
    let winner = if rows[0].3 == payload_digest(&current) {
        &current
    } else {
        &rival
    };
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, winner)
            .expect("verify winner")
    );
    let expected = [&current, &current, &current, &rival].map(|payload| payload == winner);
    assert_eq!(
        outcomes, expected,
        "exactly the winning payload's issuers succeed"
    );
}

/// The host can stop between any two writes. A reservation without a
/// certificate certifies nothing and leaves the previous certified attempt
/// current; a certificate whose run-store copy never landed is re-issued
/// unchanged after restart; and a superseded reservation never certifies.
#[test]
fn interrupted_attempts_survive_restart_without_trusting_stale_evidence() {
    let (global, workspace, recovery_a) = fixture();

    // Reserve B, then stop before the provider finishes.
    let attempt = RecoveryAuthority::open(global.path())
        .expect("open authority")
        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
        .expect("reserve recovery B");
    let recovery_b = later_attempt(
        &recovery_a,
        attempt,
        "8888888888888888888888888888888888888888",
    );

    let authority = RecoveryAuthority::open(global.path()).expect("restart");
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &recovery_a)
            .expect("verify A"),
        "an uncertified reservation does not discard A",
    );
    // The resumed run admits recovery C; B's identity can no longer certify.
    let attempt = authority
        .begin_attempt(RUN_ID, STEP_ID, &scope(workspace.path()))
        .expect("reserve recovery C");
    assert_eq!(attempt, 3);
    let error = authority
        .issue(RUN_ID, STEP_ID, &recovery_b)
        .expect_err("an interrupted attempt is superseded");
    assert!(error.to_string().contains("superseded"), "{error}");

    // C certifies, then the host stops before persisting the run-store copy.
    let recovery_c = later_attempt(
        &recovery_a,
        attempt,
        "9999999999999999999999999999999999999999",
    );
    authority
        .issue(RUN_ID, STEP_ID, &recovery_c)
        .expect("certify C");
    drop(authority);

    let authority = RecoveryAuthority::open(global.path()).expect("restart");
    assert!(
        !authority
            .verify(RUN_ID, STEP_ID, &recovery_a)
            .expect("verify A")
    );
    authority
        .issue(RUN_ID, STEP_ID, &recovery_c)
        .expect("the identical payload is re-issued after restart");
    assert!(
        authority
            .verify(RUN_ID, STEP_ID, &recovery_c)
            .expect("verify C")
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

// Linux: /proc namespace-leader binding must authenticate descendants despite forged run environment.
#[cfg(target_os = "linux")]
#[test]
fn worker_process_binding_survives_descendants_and_forged_environment() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    const CHILD_TEST: &str = "runtime::recovery_authority::tests::worker_process_binding_survives_descendants_and_forged_environment";
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
