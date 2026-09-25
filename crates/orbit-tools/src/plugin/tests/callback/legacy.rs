use super::*;

#[test]
fn token_identifies_the_minted_plugin() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = mint(root.path(), "demo");
    let _env = present_token(session.token());
    let identity = resolve_plugin_callback(root.path(), legacy_on)
        .expect("resolve")
        .expect("identified");
    assert_eq!(identity.provenance.name, "demo");
    assert_eq!(identity.provenance.version, "1.0.0");
}

#[test]
fn ancestry_identifies_the_plugin_after_the_token_is_cleared() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    session
        .bind_pid(std::process::id())
        .expect("bind this process");
    let _env = clear_token();
    let identity = resolve_plugin_callback(root.path(), legacy_on)
        .expect("resolve")
        .expect("identified by ancestry");
    assert_eq!(identity.provenance.name, "demo");
}

/// Exercise the process relationship the callback gate sees in production:
/// the Orbit process owns the session and a separately spawned backend child
/// has only its parent pid after the token is cleared.
#[cfg(unix)]
#[test]
fn a_real_child_resolves_the_plugin_through_its_parent_pid() {
    const CHILD_ENV: &str = "ORBIT_TEST_CALLBACK_ANCESTRY_CHILD";
    if let Some(root) = std::env::var_os(CHILD_ENV) {
        let _token = clear_token();
        let identity = resolve_plugin_callback(std::path::Path::new(&root), legacy_on)
            .expect("resolve in child")
            .expect("the parent session identifies the child");
        assert_eq!(identity.provenance.name, "demo");
        return;
    }

    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    session
        .bind_pid(std::process::id())
        .expect("bind the parent process");
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let test = format!("{module}::a_real_child_resolves_the_plugin_through_its_parent_pid");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &test, "--nocapture"])
        .env(CHILD_ENV, root.path())
        .env_remove(ORBIT_PLUGIN_CALLBACK_ENV)
        .output()
        .expect("spawn callback child");
    assert!(
        output.status.success(),
        "callback child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ancestry_ignores_a_live_pid_with_a_different_starttime() {
    let root = tempfile::tempdir().expect("tempdir");
    let key = orbit_common::process::ancestry::process_start_key(std::process::id())
        .expect("current process start key");
    write_record(
        root.path(),
        "reused-pid",
        key.pid,
        key.starttime.wrapping_add(1),
    );
    let _env = clear_token();

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_on).expect("resolve"),
        None
    );
}

/// A callback directory redirected outside the host root must not expose an
/// otherwise valid ancestry record from that external directory.
#[cfg(unix)]
#[test]
fn ancestry_scan_refuses_a_callback_directory_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let key = orbit_common::process::ancestry::process_start_key(std::process::id())
        .expect("current process start key");
    write_record(outside.path(), "escaped", key.pid, key.starttime);
    let state = root.path().join("state");
    std::fs::create_dir(&state).expect("create state directory");
    symlink(
        outside.path().join("state/plugin-callbacks"),
        state.join("plugin-callbacks"),
    )
    .expect("redirect callback directory outside host root");
    let _env = clear_token();

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_on).expect("resolve"),
        None
    );
}

#[test]
fn corrupt_record_does_not_hide_a_valid_ancestry_session() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    session
        .bind_pid(std::process::id())
        .expect("bind this process");
    std::fs::write(root.path().join("state/plugin-callbacks/corrupt"), b"{")
        .expect("write corrupt record");
    let _env = clear_token();

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_on)
            .expect("resolve")
            .expect("identified")
            .provenance
            .name,
        "demo"
    );
}

#[test]
fn mint_removes_dead_callback_records() {
    let root = tempfile::tempdir().expect("tempdir");
    let stale = write_record(root.path(), "dead", u32::MAX, 0);

    let _session = mint(root.path(), "demo");

    assert!(
        !stale.exists(),
        "mint must remove records for dead processes"
    );
}

#[test]
fn ordinary_callers_are_not_identified() {
    let root = tempfile::tempdir().expect("tempdir");
    let _env = clear_token();
    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_on).expect("resolve"),
        None
    );
}

#[test]
fn a_presented_unknown_token_is_a_missing_credential() {
    let root = tempfile::tempdir().expect("tempdir");
    let _env = present_token(&"ab".repeat(32));
    let error = resolve_plugin_callback(root.path(), legacy_on).expect_err("forged token");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("credential is missing or invalid"),
        "{error}"
    );
}

/// A valid token cannot redirect the retired lookup through a symlink to a
/// record outside the host callback directory.
#[cfg(unix)]
#[test]
fn retired_token_lookup_does_not_follow_a_record_symlink() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let key = orbit_common::process::ancestry::process_start_key(std::process::id())
        .expect("current process start key");
    let outside_record = write_record(outside.path(), "redirected", key.pid, key.starttime);
    let token = outside_record
        .file_name()
        .expect("token filename")
        .to_string_lossy()
        .into_owned();
    let callback_dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&callback_dir).expect("create callback directory");
    symlink(&outside_record, callback_dir.join(&token)).expect("redirect session token");
    let _env = present_token(&token);

    let error = resolve_plugin_callback(root.path(), legacy_on)
        .expect_err("the token path must not follow an external record symlink");
    assert!(matches!(error, OrbitError::Io(_)), "{error}");
}

#[test]
fn drop_removes_the_session_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let token;
    {
        let session = mint(root.path(), "demo");
        token = session.token().to_string();
        assert!(
            root.path()
                .join("state/plugin-callbacks")
                .join(&token)
                .exists()
        );
    }
    assert!(
        !root
            .path()
            .join("state/plugin-callbacks")
            .join(&token)
            .exists()
    );
}

/// A live token is a credential for the process the host bound it to, not a
/// bearer token: presenting plugin A's record from a process whose ancestry
/// names plugin B is a mismatch, never A's allowlist [ORB-12798].
#[test]
fn a_token_bound_to_another_process_is_a_mismatch() {
    let root = tempfile::tempdir().expect("tempdir");
    // Plugin B: bound to this process, so ancestry names it.
    let mut ours = mint(root.path(), "b");
    ours.bind_pid(std::process::id())
        .expect("bind this process");
    // Plugin A: bound to a live process this one is no part of — pid 1 is
    // never this process, its parent, or its group.
    let mut theirs = mint(root.path(), "a");
    theirs.bind_pid(1).expect("bind pid 1");
    let _env = present_token(theirs.token());

    let error =
        resolve_plugin_callback(root.path(), legacy_on).expect_err("A's token from B's process");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    let message = error.to_string();
    assert!(
        message.contains("'a'") && message.contains("does not match"),
        "{message}"
    );
}

/// The same binding without a second session to name: a token whose record
/// belongs to an unrelated process is refused rather than identified.
#[test]
fn a_token_whose_record_is_not_ours_is_refused_without_ancestry() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    session.bind_pid(1).expect("bind pid 1");
    let _env = present_token(session.token());

    let error =
        resolve_plugin_callback(root.path(), legacy_on).expect_err("someone else's session");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("not held by the calling process"),
        "{error}"
    );
}

/// A descendant that keeps the credential is still the backend: the bound pid
/// is this process's own, its parent's, or its process group's.
#[test]
fn a_token_bound_to_this_process_group_is_identified() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    let pgid =
        orbit_common::process::ancestry::current_process_group().expect("a process group on unix");
    session
        .bind_pid(pgid)
        .expect("bind the process group leader");
    let _env = present_token(session.token());

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_on)
            .expect("resolve")
            .expect("identified")
            .provenance
            .name,
        "demo"
    );
}

/// The session directory is host state a plugin sandbox does not grant. A
/// caller that cannot read it is inside that sandbox, so a missing credential
/// there is a refusal — not an ordinary local caller. This is what a backend
/// descendant that calls `setsid` and unsets the variable lands on.
#[cfg(unix)]
#[test]
fn an_unreadable_session_directory_with_no_credential_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000))
        .expect("make the directory unreadable");
    if std::fs::read_dir(&dir).is_ok() {
        // Root ignores directory permissions, so there is no refusal to
        // observe here. The kernel-enforced version of this case is the CLI
        // regression through the real plugin sandbox.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
        return;
    }
    let _env = clear_token();

    let error = resolve_plugin_callback(root.path(), legacy_on)
        .expect_err("confined child with no session");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("without the host-issued callback session"),
        "{error}"
    );
}
