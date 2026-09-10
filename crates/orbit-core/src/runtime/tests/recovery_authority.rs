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
/// against a live sandbox in `tests/recovery_authority_linux.rs`.
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
