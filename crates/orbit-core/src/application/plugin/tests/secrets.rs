//! A plugin call carries only its own declared secrets, and a masked
//! sandbox refuses the secret verbs.

use crate::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};

use super::super::{
    PluginAddOptions, PluginEnableOptions, PluginRemoveOptions, enable_plugin, install_plugin,
    list_plugin_secrets, remove_plugin, remove_plugin_secret, set_plugin_secret,
};
use super::fixture::{PluginFixture, PluginSpecFixture};

const TWO_SECRETS: &str = "    - name: refresh_token\n      description: OAuth refresh token.\n      rotatable: true\n    - name: api_key\n";

fn value(text: &str) -> PluginSecretValue {
    PluginSecretValue::new(text.to_string()).expect("valid secret value")
}

fn install(fixture: &PluginFixture, spec: PluginSpecFixture<'_>) {
    let source = fixture.write_plugin(spec);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install plugin");
}

fn stored_names(fixture: &PluginFixture) -> Vec<String> {
    PluginSecretStore::new(&fixture.global_root)
        .list("demo")
        .expect("list stored secrets")
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

/// Per-call delivery from the host store (design §3, "Plugin secrets"): a
/// plugin's call carries each of its own declared secrets that is set, with
/// its stored version, and nothing else — not an unset one, not a stored
/// name it no longer declares, not another plugin's secret of the same name.
/// The call's audit row names what was delivered and holds no value.
#[cfg(unix)]
#[test]
fn a_call_carries_its_own_declared_secrets_and_the_audit_row_names_them() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_call_carries_its_own_declared_secrets_and_the_audit_row_names_them",
    ) {
        return;
    }
    use serde_json::json;

    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );
    install(
        &fixture,
        PluginSpecFixture::new("other", "other").declaring_secrets("    - name: api_key\n"),
    );
    for plugin in ["demo", "other"] {
        enable_plugin(&fixture.runtime, plugin, &PluginEnableOptions::default()).expect("enable");
    }
    let status = set_plugin_secret(&fixture.runtime, "demo", "api_key", &value("demo-key-3e1"))
        .expect("set a declared secret");
    assert!(status.set);
    // A stored value under a name the manifest does not declare — what an
    // older manifest could have left — is never delivered.
    PluginSecretStore::new(&fixture.global_root)
        .put("demo", "stale_name", &value("stale-value-8d2"))
        .expect("store an undeclared name directly");
    let version = PluginSecretStore::new(&fixture.global_root)
        .get("demo", "api_key")
        .expect("read")
        .expect("set")
        .version;

    let runtime = fixture.reopen();
    let output = fixture.call(&runtime, "demo.hello").expect("demo call");
    // The backend echoes its envelope, and the caller sees delivered values
    // masked. `demo-key-3e1` is the only value this call delivered, so a
    // value that masks to exactly one marker is that value.
    assert_eq!(
        output["envelope"]["context"]["secrets"],
        json!({ "api_key": { "value": "[secret]", "version": version } }),
        "{output}"
    );
    assert!(
        !output.to_string().contains("demo-key-3e1"),
        "a delivered value reached the caller: {output}"
    );
    let other = fixture.call(&runtime, "other.hello").expect("other call");
    assert_eq!(
        other["envelope"]["context"]["secrets"],
        json!({}),
        "another plugin's `api_key` is not this plugin's: {other}"
    );

    let rows = |tool: &str| {
        runtime
            .list_audit_events(None, Some(tool.to_string()), None, None, 10)
            .expect("audit events")
    };
    let demo_rows = rows("demo.hello");
    assert_eq!(demo_rows.len(), 1);
    assert_eq!(demo_rows[0].plugin_secrets, vec!["api_key".to_string()]);
    let other_rows = rows("other.hello");
    assert!(other_rows[0].plugin_secrets.is_empty());
    for row in demo_rows.iter().chain(&other_rows) {
        let text = serde_json::to_string(row).expect("serialize audit row");
        assert!(
            !text.contains("demo-key-3e1") && !text.contains("stale-value-8d2"),
            "an audit row holds no value: {text}"
        );
    }
}

/// What an operator command sees from inside an agent sandbox: the Linux
/// sentinel standing in for the secret store.
fn mask_secret_store(fixture: &PluginFixture) -> std::path::PathBuf {
    let sentinel = crate::runtime::plugin::paths::plugin_secret_store_dir(&fixture.global_root)
        .join(crate::runtime::plugin::sandbox_mask::PLUGIN_MASK_SENTINEL_FILE);
    std::fs::create_dir_all(sentinel.parent().expect("store dir")).expect("store dir");
    std::fs::write(&sentinel, b"masked").expect("sentinel");
    sentinel
}

fn assert_not_visible<T: std::fmt::Debug>(result: Result<T, orbit_common::OrbitError>) {
    match result {
        Err(orbit_common::OrbitError::PolicyDenied(message)) => assert!(
            message.contains("not visible from an agent sandbox"),
            "{message}"
        ),
        other => panic!("expected a masked-sandbox refusal, got {other:?}"),
    }
}

/// Inside a masked sandbox the secret verbs and every removal that would
/// delete secrets or state refuse, and change nothing a host run would see.
#[test]
fn a_masked_sandbox_refuses_secret_verbs_and_removal_without_changing_anything() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_masked_sandbox_refuses_secret_verbs_and_removal_without_changing_anything",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );
    set_plugin_secret(&fixture.runtime, "demo", "api_key", &value("x")).expect("set");
    let sentinel = mask_secret_store(&fixture);

    assert_not_visible(list_plugin_secrets(&fixture.runtime, "demo"));
    assert_not_visible(set_plugin_secret(
        &fixture.runtime,
        "demo",
        "refresh_token",
        &value("y"),
    ));
    assert_not_visible(remove_plugin_secret(&fixture.runtime, "demo", "api_key"));
    for options in [
        PluginRemoveOptions::default(),
        PluginRemoveOptions {
            purge_state: true,
            ..PluginRemoveOptions::default()
        },
    ] {
        assert_not_visible(remove_plugin(&fixture.runtime, "demo", &options));
    }

    std::fs::remove_file(&sentinel).expect("lift the mask");
    assert_eq!(stored_names(&fixture), ["api_key"]);
    assert!(
        super::super::show_plugin(&fixture.reopen(), "demo").is_ok(),
        "the plugin is still installed"
    );
}
