use orbit_common::OrbitError;
use orbit_types::plugin::PluginProvenance;

use super::super::callback::{
    ORBIT_PLUGIN_CALLBACK_ENV, PluginCallbackSession, resolve_plugin_callback,
};

fn provenance(name: &str) -> PluginProvenance {
    PluginProvenance {
        name: name.into(),
        version: "1.0.0".into(),
        manifest_digest: "abc".into(),
        grants: vec!["orbit_tools".into()],
    }
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
            r#"{{"schema_version":1,"plugin":"stale","version":"1.0.0","manifest_digest":"abc","pid":{pid},"starttime":{starttime}}}"#
        ),
    )
    .expect("write callback record");
    path
}

#[test]
fn token_identifies_the_minted_plugin() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = PluginCallbackSession::mint(root.path(), &provenance("demo")).expect("mint");
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
    let mut session = PluginCallbackSession::mint(root.path(), &provenance("demo")).expect("mint");
    session
        .bind_pid(std::process::id())
        .expect("bind this process");
    let _env = clear_token();
    let identity = resolve_plugin_callback(root.path())
        .expect("resolve")
        .expect("identified by ancestry");
    assert_eq!(identity.name, "demo");
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
    let mut session = PluginCallbackSession::mint(root.path(), &provenance("demo")).expect("mint");
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

    let _session = PluginCallbackSession::mint(root.path(), &provenance("demo")).expect("mint");

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
        let session = PluginCallbackSession::mint(root.path(), &provenance("demo")).expect("mint");
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
