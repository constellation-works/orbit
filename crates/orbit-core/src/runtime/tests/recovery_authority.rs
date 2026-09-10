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
fn evidence_survives_restart_and_stays_bound_to_its_run() {
    let (global, workspace, accepted) = fixture();

    // Reopening from disk is the host restart: the authority is durable, and a
    // replayed copy is still refused under any other identity.
    let restarted = RecoveryAuthority::open(global.path()).expect("reopen after restart");
    assert!(
        restarted
            .verify(RUN_ID, STEP_ID, &accepted)
            .expect("verify after restart"),
    );

    let replayed = checkpoint(OTHER_RUN_ID, STEP_ID, workspace.path());
    assert!(
        !restarted
            .verify(OTHER_RUN_ID, STEP_ID, &replayed)
            .expect("verify replay"),
        "a replayed payload has no certificate of its own",
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

#[test]
fn evidence_must_name_the_run_and_step_it_is_certified_under() {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    let authority = RecoveryAuthority::open(global.path()).expect("open authority");

    let error = authority
        .issue(
            OTHER_RUN_ID,
            STEP_ID,
            &checkpoint(RUN_ID, STEP_ID, workspace.path()),
        )
        .expect_err("mismatched identity must not be certified");
    assert!(error.to_string().contains("not run"), "{error}");

    let mut incomplete = checkpoint(RUN_ID, STEP_ID, workspace.path());
    incomplete
        .as_object_mut()
        .expect("object payload")
        .remove("base_sha");
    let error = authority
        .issue(RUN_ID, STEP_ID, &incomplete)
        .expect_err("unbound evidence must not be certified");
    assert!(error.to_string().contains("`base_sha`"), "{error}");
    assert!(
        !authority
            .verify(RUN_ID, STEP_ID, &incomplete)
            .expect("verify unbound"),
    );
}

/// An uncertified run-store row is exactly the shape a pre-boundary checkpoint
/// has. Nothing blesses it: the authority reports no record, and the resume
/// path turns that into a refusal.
#[test]
fn checkpoints_written_before_the_boundary_have_no_record() {
    let global = TempDir::new().expect("global root");
    let workspace = TempDir::new().expect("workspace");
    let authority = RecoveryAuthority::open(global.path()).expect("open authority");

    assert!(
        !authority
            .verify(
                RUN_ID,
                STEP_ID,
                &checkpoint(RUN_ID, STEP_ID, workspace.path())
            )
            .expect("verify legacy row"),
    );
}

/// Rule *shape*: the deny lands last and is a subtree, which is what makes it
/// cover `authority.db` together with the `-wal` and `-shm` sidecars a writer
/// could otherwise use to inject rows. Enforcement of these rules is proven
/// against a live sandbox in
/// `adapter/engine_host/v2_host/tests/recovery_authority_sandbox.rs`.
#[test]
fn the_authority_deny_is_a_subtree_rule_appended_after_every_grant() {
    let global = TempDir::new().expect("global root");
    let leaf_grants = vec![
        format!("{}/tasks", global.path().display()),
        format!("{}/orbit.db", global.path().display()),
        format!("{}/orbit.db-wal", global.path().display()),
    ];
    let mut resolved = ResolvedFsProfile {
        name: "implementer".to_string(),
        read: vec!["/**".to_string()],
        modify: leaf_grants.clone(),
    };
    append_recovery_authority_denies(global.path(), &mut resolved).expect("append denies");

    let root = global
        .path()
        .canonicalize()
        .expect("canonical global root")
        .join("state/recovery-authority");
    assert!(root.is_dir(), "the protected root is created on demand");

    let deny = format!("!{}/**", root.display());
    assert_eq!(
        resolved.modify.last(),
        Some(&deny),
        "the deny must come after every grant so a later grant cannot win",
    );
    assert!(
        resolved.modify.starts_with(&leaf_grants),
        "leaf task and run-store grants are left untouched",
    );

    // Appending twice is idempotent, so repeated resolution cannot accumulate
    // duplicate rules.
    append_recovery_authority_denies(global.path(), &mut resolved).expect("append denies again");
    assert_eq!(
        resolved.modify.iter().filter(|rule| *rule == &deny).count(),
        1
    );
}

/// The configured global root may legitimately be reached through a symlink —
/// a symlinked `$HOME` is a supported layout — so it is resolved to its trusted
/// target rather than refused. What must hold is that resolution is *stable*:
/// both spellings name one authority, so a certificate issued through the alias
/// is the same record the canonical path reads back.
#[cfg(unix)]
#[test]
fn a_symlinked_global_root_resolves_to_one_trusted_authority() {
    use std::os::unix::fs::symlink;

    let base = TempDir::new().expect("base");
    let workspace = TempDir::new().expect("workspace");
    let real = base.path().join("real-global");
    let alias = base.path().join("aliased-global");
    std::fs::create_dir(&real).expect("real global root");
    symlink(&real, &alias).expect("global root symlink");

    let accepted = checkpoint(RUN_ID, STEP_ID, workspace.path());
    RecoveryAuthority::open(&alias)
        .expect("open through the alias")
        .issue(RUN_ID, STEP_ID, &accepted)
        .expect("issue through the alias");

    assert!(
        real.join("state/recovery-authority/authority.db").is_file(),
        "the authority must land under the trusted target, not beside the link",
    );
    assert!(
        !alias.symlink_metadata().expect("alias metadata").is_dir(),
        "the alias itself must stay a symlink rather than be replaced",
    );
    assert!(
        RecoveryAuthority::open(&real)
            .expect("open through the trusted target")
            .verify(RUN_ID, STEP_ID, &accepted)
            .expect("verify through the trusted target"),
        "both spellings must resolve to the same certificate",
    );
}

/// The same property one level up: an aliased *ancestor* of the global root
/// resolves to the trusted target instead of forking the authority in two.
#[cfg(unix)]
#[test]
fn a_symlinked_ancestor_of_the_global_root_resolves_to_the_same_authority() {
    use std::os::unix::fs::symlink;

    let base = TempDir::new().expect("base");
    let workspace = TempDir::new().expect("workspace");
    let real_parent = base.path().join("real-parent");
    std::fs::create_dir_all(real_parent.join("global")).expect("global root");
    symlink(&real_parent, base.path().join("aliased-parent")).expect("ancestor symlink");

    let accepted = checkpoint(RUN_ID, STEP_ID, workspace.path());
    RecoveryAuthority::open(&base.path().join("aliased-parent/global"))
        .expect("open through the aliased ancestor")
        .issue(RUN_ID, STEP_ID, &accepted)
        .expect("issue through the aliased ancestor");

    assert!(
        real_parent
            .join("global/state/recovery-authority/authority.db")
            .is_file(),
        "the authority must land under the trusted target of the aliased ancestor",
    );
    assert!(
        RecoveryAuthority::open(&real_parent.join("global"))
            .expect("open through the trusted target")
            .verify(RUN_ID, STEP_ID, &accepted)
            .expect("verify through the trusted target"),
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

/// The configured root is untrusted input, so a shape that would resolve
/// against the process working directory — or climb out of itself — is refused
/// before it can select where authority state lives.
#[test]
fn an_unanchored_global_root_is_refused() {
    for (label, root) in [
        ("relative", Path::new("relative/global").to_path_buf()),
        ("traversing", Path::new("/tmp/../tmp/global").to_path_buf()),
    ] {
        let error = RecoveryAuthority::open(&root).expect_err("an unanchored root must fail");
        assert!(
            matches!(error, orbit_common::OrbitError::InvalidInput(_)),
            "a `{label}` root must be refused as invalid input: {error}",
        );
    }

    let base = TempDir::new().expect("base");
    let error = RecoveryAuthority::open(&base.path().join("never-created"))
        .expect_err("a missing root must fail");
    assert!(
        error
            .to_string()
            .contains("resolve recovery authority root"),
        "{error}",
    );
}
