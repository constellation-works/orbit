//! Ruleset opening: an enumerated grant that vanished is skipped, a named
//! grant that is missing is refused. [F2026-11-206]

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use super::super::grants::{LandlockGrant, LandlockPathGrant};
use super::super::probe::RulesetScope;
use super::super::ruleset::{Ruleset, abi_version};

fn ruleset() -> Option<Ruleset> {
    let abi = abi_version();
    if abi < super::super::probe::MINIMUM_LANDLOCK_ABI {
        return None;
    }
    Some(Ruleset::create(RulesetScope::default(), abi).expect("create ruleset"))
}

fn grant(path: PathBuf, allow_vanished: bool) -> LandlockPathGrant {
    LandlockPathGrant {
        path,
        grant: LandlockGrant::ReadTree,
        allow_vanished,
    }
}

/// A sibling `TMPDIR/.tmpXXXX` another owner removes between the walk that
/// found it and the ruleset open must not fail the spawn: compile a grant
/// for a shared-directory sibling, remove it, then open the ruleset.
#[test]
fn a_vanished_enumerated_grant_is_skipped_when_the_ruleset_opens() {
    let Some(ruleset) = ruleset() else {
        return;
    };
    let shared = tempfile::tempdir().expect("shared TMPDIR");
    let sibling = shared.path().join(".tmpSibling");
    std::fs::create_dir(&sibling).expect("sibling dir");
    // Found by enumeration, then removed before the ruleset opens — the
    // deterministic form of the sibling race, with no parallel scheduling.
    let enumerated = grant(sibling.clone(), true);
    std::fs::remove_dir(&sibling).expect("remove sibling before ruleset opens");
    ruleset
        .add_path(&enumerated)
        .expect("a vanished enumerated grant grants nothing and must be skipped");
}

/// The same missing path named by the policy itself must still be refused,
/// not silently dropped.
#[test]
fn a_missing_named_grant_is_refused_when_the_ruleset_opens() {
    let Some(ruleset) = ruleset() else {
        return;
    };
    let shared = tempfile::tempdir().expect("shared TMPDIR");
    let missing = shared.path().join("policy-named-missing");
    let error = ruleset
        .add_path(&grant(missing.clone(), false))
        .expect_err("a missing policy-named grant must be refused");
    assert!(
        error
            .to_string()
            .contains(&format!("open landlock grant `{}`", missing.display())),
        "{error}"
    );
}

/// Grants compiled from the plugin boundary for a shared `TMPDIR` mark the
/// sibling entries found by walking it as enumerated, while the decision
/// store inside the denied tree stays ungranted.
#[test]
fn boundary_grants_mark_shared_tmpdir_siblings_enumerated_and_keep_the_store_denied() {
    use crate::{
        EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode, grants_read,
        linux_landlock_boundary_grants,
    };

    // Shared TMPDIR holding both a foreign sibling and this fixture's own
    // tree, which contains a global root with a denied store beneath it:
    // compiling the host TMPDIR grant then has to walk the shared directory.
    let shared = tempfile::tempdir().expect("shared TMPDIR");
    let shared = shared.path().canonicalize().expect("canonical shared");
    let sibling = shared.join(".tmpSibling");
    std::fs::create_dir(&sibling).expect("sibling dir");
    std::fs::write(sibling.join("public.txt"), "SIBLING").expect("sibling file");
    let global_root = shared.join(".tmpOwner/orbit-global");
    let store = global_root.join("state/decision-channels");
    std::fs::create_dir_all(&store).expect("store dir");
    let canary = store.join("channels.json");
    std::fs::write(&canary, "CHANNEL-STORE-CANARY").expect("canary");

    let request = ExecRequest {
        program: "/bin/true".to_string(),
        args: vec![],
        current_dir: None,
        timeout_ms: None,
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TMPDIR".to_string(), shared.display().to_string()),
        ]),
        debug: false,
    };
    let boundary = LandlockBoundary {
        read: vec![global_root.clone()],
        read_denies: vec![store.clone()],
        ..LandlockBoundary::default()
    };
    let grants = linux_landlock_boundary_grants(&request, &boundary).expect("compile boundary");

    let sibling_grant = grants
        .iter()
        .find(|grant| grant.path == sibling)
        .expect("the shared TMPDIR walk grants the sibling it found");
    assert!(
        sibling_grant.allow_vanished,
        "a sibling found by walking a shared directory must tolerate vanishing: {sibling_grant:?}"
    );
    assert!(
        !grants_read(&grants, &canary),
        "the decision-channel store must stay ungranted: {grants:?}"
    );
    assert!(
        grants_read(&grants, &sibling.join("public.txt")),
        "the sibling itself stays readable while it exists: {grants:?}"
    );
}
