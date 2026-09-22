use orbit_common::OrbitError;
use orbit_types::plugin::PluginProvenance;

use super::super::callback::{
    ORBIT_PLUGIN_CALLBACK_ENV, PluginCallbackSession, resolve_plugin_callback,
    stale_plugin_callback_session_count,
};

fn provenance(name: &str) -> PluginProvenance {
    PluginProvenance {
        name: name.into(),
        version: "1.0.0".into(),
        manifest_digest: "abc".into(),
        grants: vec!["orbit_tools".into()],
    }
}

/// Mint a session whose ceiling is the plugin's full requested list; the
/// ceiling's own behaviour has its own tests below.
fn mint(root: &std::path::Path, name: &str) -> PluginCallbackSession {
    PluginCallbackSession::mint(root, &provenance(name), &["orbit.task.list".to_string()])
        .expect("mint")
}

fn present_token(token: &str) -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([(ORBIT_PLUGIN_CALLBACK_ENV, Some(token))])
}

fn clear_token() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([(ORBIT_PLUGIN_CALLBACK_ENV, None)])
}

fn write_record(
    root: &std::path::Path,
    name: &str,
    pid: u32,
    starttime: u64,
) -> std::path::PathBuf {
    let dir = root.join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            r#"{{"schema_version":2,"plugin":"stale","version":"1.0.0","manifest_digest":"abc","effective_tools":[],"pid":{pid},"starttime":{starttime}}}"#
        ),
    )
    .expect("write callback record");
    path
}

#[test]
fn token_identifies_the_minted_plugin() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = mint(root.path(), "demo");
    let _env = present_token(session.token());
    let identity = resolve_plugin_callback(root.path())
        .expect("resolve")
        .expect("identified");
    assert_eq!(identity.name, "demo");
    assert_eq!(identity.version, "1.0.0");
}

#[test]
fn ancestry_identifies_the_plugin_after_the_token_is_cleared() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    session
        .bind_pid(std::process::id())
        .expect("bind this process");
    let _env = clear_token();
    let identity = resolve_plugin_callback(root.path())
        .expect("resolve")
        .expect("identified by ancestry");
    assert_eq!(identity.name, "demo");
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
        let identity = resolve_plugin_callback(std::path::Path::new(&root))
            .expect("resolve in child")
            .expect("the parent session identifies the child");
        assert_eq!(identity.name, "demo");
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

    assert_eq!(resolve_plugin_callback(root.path()).expect("resolve"), None);
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
        resolve_plugin_callback(root.path())
            .expect("resolve")
            .expect("identified")
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
    assert_eq!(resolve_plugin_callback(root.path()).expect("resolve"), None);
}

#[test]
fn a_presented_unknown_token_is_a_missing_credential() {
    let root = tempfile::tempdir().expect("tempdir");
    let _env = present_token(&"ab".repeat(32));
    let error = resolve_plugin_callback(root.path()).expect_err("forged token");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("credential is missing or invalid"),
        "{error}"
    );
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

    let error = resolve_plugin_callback(root.path()).expect_err("A's token from B's process");
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

    let error = resolve_plugin_callback(root.path()).expect_err("someone else's session");
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
        resolve_plugin_callback(root.path())
            .expect("resolve")
            .expect("identified")
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

    let error = resolve_plugin_callback(root.path()).expect_err("confined child with no session");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("without the host-issued callback session"),
        "{error}"
    );
}

/// The record carries authority as well as identity: the spawning caller's
/// effective intersection, which dispatch intersects with the recorded
/// allowlist on every callback [ORB-12801].
#[test]
fn the_session_carries_the_callers_effective_tool_ceiling() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        // Unsorted and repeated on purpose: the recorded ceiling must not
        // depend on how the caller happened to spell its own allowlist.
        &[
            "orbit.task.list".to_string(),
            "orbit.search".to_string(),
            "orbit.task.list".to_string(),
        ],
    )
    .expect("mint");
    assert_eq!(
        session.effective_tools(),
        ["orbit.search".to_string(), "orbit.task.list".to_string()]
    );
    let _env = present_token(session.token());

    let identity = resolve_plugin_callback(root.path())
        .expect("resolve")
        .expect("identified");
    assert_eq!(
        identity.effective_tools,
        ["orbit.search".to_string(), "orbit.task.list".to_string()]
    );
    assert!(identity.ceiling_admits("orbit.search"));
    assert!(!identity.ceiling_admits("orbit.task.update"));
}

/// Two live sessions of the same plugin, minted for callers with different
/// allowlists. A token resolves to its own record's ceiling, not to whichever
/// session happens to be listed first.
#[test]
fn each_token_resolves_to_its_own_ceiling() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut narrow = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        &["orbit.task.list".to_string()],
    )
    .expect("mint narrow");
    narrow
        .bind_pid(std::process::id())
        .expect("bind this process");
    let mut wide = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        &["orbit.task.list".to_string(), "orbit.search".to_string()],
    )
    .expect("mint wide");
    wide.bind_pid(std::process::id())
        .expect("bind this process");

    let ceiling_for = |session: &PluginCallbackSession| {
        let _env = present_token(session.token());
        resolve_plugin_callback(root.path())
            .expect("resolve")
            .expect("identified")
            .effective_tools
    };
    assert_eq!(ceiling_for(&narrow), ["orbit.task.list".to_string()]);
    assert_eq!(
        ceiling_for(&wide),
        ["orbit.search".to_string(), "orbit.task.list".to_string()]
    );
}

/// A record that states no ceiling is not a session with an unbounded one. It
/// can only be a leftover from a host that predates the ceiling, so it is
/// refused rather than read as authority nobody granted.
#[test]
fn a_record_without_a_ceiling_is_not_a_session() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    let token = "ab".repeat(32);
    std::fs::write(
        dir.join(&token),
        br#"{"schema_version":1,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","pid":0,"starttime":0}"#,
    )
    .expect("write a pre-ceiling record");
    let _env = present_token(&token);

    let error = resolve_plugin_callback(root.path()).expect_err("a record with no ceiling");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("credential is missing or invalid"),
        "{error}"
    );
}

/// The janitor's half of refusing a pre-ceiling record. The leftover here
/// names a *live* process with its real start time, so the only thing that
/// makes it not a session is its schema version: it is never honoured, it is
/// counted stale, and the next backend start sweeps it. Skipping it in the
/// janitor's view instead would strand a mode-0600 file under the session
/// directory that no surface reports and no sweep removes [ORB-12879].
#[test]
fn a_pre_ceiling_record_is_never_honoured_but_is_counted_stale_and_swept() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    let key = orbit_common::process::ancestry::process_start_key(std::process::id())
        .expect("current process start key");
    let leftover = dir.join("ab".repeat(32));
    std::fs::write(
        &leftover,
        format!(
            r#"{{"schema_version":1,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","pid":{},"starttime":{}}}"#,
            key.pid, key.starttime
        ),
    )
    .expect("write a pre-ceiling record");

    {
        // Ancestry would name this very process if the record parsed at all.
        let _env = clear_token();
        assert_eq!(
            resolve_plugin_callback(root.path()).expect("resolve"),
            None,
            "a pre-ceiling record must never identify a caller"
        );
    }
    assert_eq!(
        stale_plugin_callback_session_count(root.path()).expect("count stale"),
        1,
        "a record this host cannot read is stale, not invisible"
    );

    // Minting is what a backend start does: it sweeps before it writes.
    let _session = mint(root.path(), "demo");

    assert!(
        !leftover.exists(),
        "the backend-start sweep must remove a pre-ceiling record"
    );
    assert_eq!(
        stale_plugin_callback_session_count(root.path()).expect("count stale"),
        0,
        "the freshly minted session is not itself stale"
    );
}

/// The sweep must not reap a record it caught mid-write. Both `mint` and
/// `bind_pid` leave the file empty between opening it and the single write
/// that fills it, and unlinking one there would destroy a live session —
/// along with the Landlock grant its child reads the record through.
#[test]
fn a_partially_written_record_is_not_counted_stale() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    for (name, bytes) in [
        ("just-created", &b""[..]),
        ("half-written", &br#"{"schema_version":2,"plugin":"de"#[..]),
    ] {
        std::fs::write(dir.join(name), bytes).expect("write a partial record");
    }

    assert_eq!(
        stale_plugin_callback_session_count(root.path()).expect("count stale"),
        0,
        "ownership of a partial record cannot be established, so it is left alone"
    );
}
