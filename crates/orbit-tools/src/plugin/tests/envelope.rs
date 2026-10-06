//! What both dispatch surfaces tell a backend about the call, and what they
//! must never tell anything else.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginGrant, PluginPermissions, PluginSecretUpdateStatus};
use serde_json::json;

use super::super::backend::{
    DeliveredPluginSecret, PluginBackendSpec, PluginConfigSection, PluginSecretDelivery,
    PluginSecretSource,
};
use super::super::envelope::{CallSecrets, apply_secret_updates, take_plugin_secret_updates};
use super::support::{CasSource, capture_logs, spec};

/// A backend spec whose plugin is configured: `[plugins.demo]` over the
/// manifest's defaults, already validated by the host.
fn configured_spec(root: &Path) -> PluginBackendSpec {
    let mut spec = (*spec(
        root.join("bin/backend.sh"),
        root,
        PluginPermissions::default(),
        &[PluginGrant::Fs],
    ))
    .clone();
    spec.config = PluginConfigSection::new(json!({
        "index_dir": "/srv/graph",
        "max_nodes": 500,
        "incremental": true,
        "api_token": "s3cret",
    }));
    spec
}

/// A source holding values for more names than any one plugin declares, and
/// recording which names it was asked for.
struct RecordingSource {
    values: BTreeMap<String, DeliveredPluginSecret>,
}

impl RecordingSource {
    fn holding(values: &[(&str, &str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            values: values
                .iter()
                .map(|(name, value, version)| {
                    (
                        (*name).to_string(),
                        DeliveredPluginSecret {
                            value: (*value).to_string(),
                            version: (*version).to_string(),
                        },
                    )
                })
                .collect(),
        })
    }
}

impl PluginSecretSource for RecordingSource {
    fn read(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, DeliveredPluginSecret>, OrbitError> {
        let _ = names;
        // Deliberately answers with everything it holds, asked for or not:
        // the delivery must bound the result itself.
        Ok(self.values.clone())
    }
}

/// A spec declaring `api_token` and `refresh_token`, over a source that also
/// holds a value for a name the plugin never declared.
fn secret_spec(root: &Path, source: Arc<RecordingSource>) -> PluginBackendSpec {
    let mut spec = configured_spec(root);
    spec.secrets = PluginSecretDelivery::new(
        vec!["api_token".to_string(), "refresh_token".to_string()],
        source,
    );
    spec
}

#[test]
fn a_delivered_value_in_an_update_name_cannot_reach_diagnostics_or_audit() {
    // ORB-14389: tracing-core's single-dispatcher fast path registers a
    // callsite using the current thread's subscriber. A parallel call to
    // apply_secret_updates without our scoped subscriber can cache `never`
    // for the shared warning and suppress it here. Own the process so this
    // test registers and observes the warning without sibling interference.
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .expect("test module belongs to this crate");
    let test =
        format!("{module}::a_delivered_value_in_an_update_name_cannot_reach_diagnostics_or_audit");
    if std::env::var("ORBIT_TEST_PLUGIN_DIAGNOSTIC_CHILD").as_deref() != Ok(test.as_str()) {
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", &test, "--nocapture"])
            .env("ORBIT_TEST_PLUGIN_DIAGNOSTIC_CHILD", &test);
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            std::time::Duration::from_secs(30),
            64 * 1024,
        )
        .expect("run isolated diagnostic test");
        orbit_common::test_env::assert_child_test_passed(
            &test,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    // Valid secret-name syntax alone cannot prevent an opaque value being
    // smuggled into the backend-controlled name of a refused update.
    let value = "opaque-review-canary-8d44d9";
    let spec = secret_spec(
        temp.path(),
        RecordingSource::holding(&[("api_token", value, "v1")]),
    );
    let secrets = CallSecrets::resolve(&spec).expect("resolve");
    let (outcomes, logs) = capture_logs(|| {
        apply_secret_updates(
            &spec,
            "demo.hello",
            &secrets,
            Some(&json!({value:{"value":"unused","expected_version":null}})),
        )
    });
    assert_eq!(
        outcomes,
        BTreeMap::from([("[secret]".into(), PluginSecretUpdateStatus::Refused)])
    );
    assert_eq!(take_plugin_secret_updates(), outcomes);
    assert!(
        !logs.contains(value),
        "delivered value reached a diagnostic: {logs}"
    );
    assert!(
        logs.contains("does not declare it"),
        "cause preserved: {logs}"
    );
}

/// A spec declaring `refresh_token` (rotatable) and `api_key` (not), over
/// an in-memory store with the host's compare-and-swap.
fn rotation_spec(root: &Path, source: Arc<CasSource>) -> PluginBackendSpec {
    let mut spec = configured_spec(root);
    spec.secrets = PluginSecretDelivery::new(
        vec!["refresh_token".to_string(), "api_key".to_string()],
        source,
    )
    .with_rotatable(vec!["refresh_token".to_string()]);
    spec
}

/// Two calls delivered the same version both rotate from it: the store's
/// compare-and-swap applies exactly one, and the other is refused rather
/// than overwriting the winner.
#[test]
fn two_updates_from_the_same_version_apply_exactly_one() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = CasSource::holding(&[("refresh_token", "old-token-11a", "v1")]);
    let spec = Arc::new(rotation_spec(temp.path(), Arc::clone(&source)));
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let racers: Vec<_> = ["token-from-a-3c", "token-from-b-4d"]
        .into_iter()
        .map(|value| {
            let spec = Arc::clone(&spec);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let outcome = apply_secret_updates(
                    &spec,
                    "demo.hello",
                    &CallSecrets::default(),
                    Some(&json!({
                        "refresh_token": { "value": value, "expected_version": "v1" },
                    })),
                );
                (value, outcome["refresh_token"])
            })
        })
        .collect();
    let results: Vec<_> = racers
        .into_iter()
        .map(|racer| racer.join().expect("racer"))
        .collect();

    let winners: Vec<&str> = results
        .iter()
        .filter(|(_, status)| *status == PluginSecretUpdateStatus::Applied)
        .map(|(value, _)| *value)
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert!(
        results
            .iter()
            .any(|(_, status)| *status == PluginSecretUpdateStatus::Refused),
        "{results:?}"
    );
    assert_eq!(
        source.stored("refresh_token").expect("stored").0,
        winners[0],
        "the stored value is the winner's"
    );
}
