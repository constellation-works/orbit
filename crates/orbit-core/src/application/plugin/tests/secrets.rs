//! Sibling tests for `secrets.rs`: operator `set|list|rm` against the
//! installed manifest's `spec.secrets`, and what `enable`, `upgrade`,
//! `remove` and `doctor` do with a plugin's secrets.

use crate::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};

use super::super::{
    PluginAddOptions, PluginEnableOptions, PluginRemoveOptions, PluginUpgradeOptions,
    enable_plugin, install_plugin, list_plugin_secrets, plugin_doctor, remove_plugin,
    remove_plugin_secret, set_plugin_secret, upgrade_plugin,
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

#[test]
fn set_accepts_only_a_declared_name_and_list_shows_set_state() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );

    let error = set_plugin_secret(&fixture.runtime, "demo", "other", &value("x"))
        .expect_err("undeclared name");
    assert!(
        error
            .to_string()
            .contains("does not declare a secret named 'other'")
            && error.to_string().contains("refresh_token, api_key"),
        "{error}"
    );
    assert!(stored_names(&fixture).is_empty());

    let status = set_plugin_secret(&fixture.runtime, "demo", "api_key", &value("key"))
        .expect("set a declared secret");
    assert!(status.set && status.declared && status.updated_at.is_some());

    let listed = list_plugin_secrets(&fixture.runtime, "demo").expect("list");
    let summary: Vec<(&str, bool, bool)> = listed
        .iter()
        .map(|secret| (secret.name.as_str(), secret.set, secret.rotatable))
        .collect();
    assert_eq!(
        summary,
        [("refresh_token", false, true), ("api_key", true, false)]
    );
}

#[test]
fn set_needs_an_installed_plugin() {
    let fixture = PluginFixture::new();
    let error = set_plugin_secret(&fixture.runtime, "absent", "token", &value("x"))
        .expect_err("no such plugin");
    assert!(error.to_string().contains("not installed"), "{error}");
}

#[test]
fn rm_removes_one_secret_and_reports_an_unset_one() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );
    for name in ["refresh_token", "api_key"] {
        set_plugin_secret(&fixture.runtime, "demo", name, &value("x")).expect("set");
    }

    assert!(remove_plugin_secret(&fixture.runtime, "demo", "api_key").expect("rm"));
    assert!(!remove_plugin_secret(&fixture.runtime, "demo", "api_key").expect("rm again"));
    assert_eq!(stored_names(&fixture), ["refresh_token"]);
    assert!(remove_plugin_secret(&fixture.runtime, "demo", "Bad Name").is_err());
}

#[test]
fn enable_names_each_unset_declared_secret() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );
    set_plugin_secret(&fixture.runtime, "demo", "api_key", &value("x")).expect("set");

    let result =
        enable_plugin(&fixture.runtime, "demo", &PluginEnableOptions::default()).expect("enable");
    let unset: Vec<&String> = result
        .warnings
        .iter()
        .filter(|warning| warning.contains("is declared but not set"))
        .collect();
    assert_eq!(unset.len(), 1, "{:?}", result.warnings);
    assert!(
        unset[0].contains("`refresh_token`")
            && unset[0].contains("orbit plugin secret set demo refresh_token"),
        "{unset:?}"
    );
}

#[test]
fn upgrade_keeps_still_declared_secrets_and_drops_the_rest() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo-v1", "demo").declaring_secrets(TWO_SECRETS),
    );
    set_plugin_secret(&fixture.runtime, "demo", "refresh_token", &value("keep")).expect("set");
    set_plugin_secret(&fixture.runtime, "demo", "api_key", &value("drop")).expect("set");
    let before = PluginSecretStore::new(&fixture.global_root)
        .get("demo", "refresh_token")
        .expect("get")
        .expect("stored");

    let mut v2 = PluginSpecFixture::new("demo-v2", "demo")
        .declaring_secrets("    - name: refresh_token\n      rotatable: true\n");
    v2.version = "2.0.0";
    let v2 = fixture.write_plugin(v2);
    upgrade_plugin(
        &fixture.runtime,
        "demo",
        Some(v2.to_str().expect("utf8 path")),
        &PluginUpgradeOptions::default(),
    )
    .expect("upgrade");

    assert_eq!(stored_names(&fixture), ["refresh_token"]);
    let after = PluginSecretStore::new(&fixture.global_root)
        .get("demo", "refresh_token")
        .expect("get")
        .expect("kept");
    assert_eq!(after, before, "a kept secret keeps its value and version");
}

#[test]
fn remove_deletes_the_secrets_and_record_only_keeps_them() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );
    set_plugin_secret(&fixture.runtime, "demo", "api_key", &value("x")).expect("set");

    remove_plugin(
        &fixture.runtime,
        "demo",
        &PluginRemoveOptions {
            record_only: true,
            ..PluginRemoveOptions::default()
        },
    )
    .expect("record-only remove");
    assert_eq!(stored_names(&fixture), ["api_key"]);

    // The files a record-only removal left in place are reinstalled over.
    let source = fixture.sources.join("demo");
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            force: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("reinstall");
    assert_eq!(
        stored_names(&fixture),
        ["api_key"],
        "a reinstall keeps a still-declared secret"
    );
    remove_plugin(&fixture.runtime, "demo", &PluginRemoveOptions::default()).expect("remove");
    assert!(stored_names(&fixture).is_empty());
}

#[test]
fn doctor_reports_each_declared_but_unset_secret() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").declaring_secrets(TWO_SECRETS),
    );
    set_plugin_secret(&fixture.runtime, "demo", "refresh_token", &value("x")).expect("set");

    let rows = plugin_doctor(&fixture.reopen()).expect("doctor");
    let secret_rows: Vec<&str> = rows
        .iter()
        .filter(|row| row.message.contains("declares secret"))
        .map(|row| row.message.as_str())
        .collect();
    assert_eq!(secret_rows.len(), 1, "{rows:?}");
    assert!(
        secret_rows[0].contains("`api_key`")
            && secret_rows[0].contains("orbit plugin secret set demo api_key"),
        "{secret_rows:?}"
    );
}

/// Per-call delivery from the host store (design §3, "Plugin secrets"): a
/// plugin's call carries each of its own declared secrets that is set, with
/// its stored version, and nothing else — not an unset one, not a stored
/// name it no longer declares, not another plugin's secret of the same name.
/// The call's audit row names what was delivered and holds no value.
#[cfg(unix)]
#[test]
fn a_call_carries_its_own_declared_secrets_and_the_audit_row_names_them() {
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
    assert_eq!(
        output["envelope"]["context"]["secrets"],
        json!({ "api_key": { "value": "demo-key-3e1", "version": version } }),
        "{output}"
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
