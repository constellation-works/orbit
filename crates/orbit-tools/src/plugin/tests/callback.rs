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
