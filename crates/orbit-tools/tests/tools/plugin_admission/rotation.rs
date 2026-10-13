//! Concurrent rotations through the public response-update boundary.
use super::support::{CasSource, context, spec, stub_backend, tool};
use orbit_tools::{
    Tool,
    plugin::{PluginBackendSpec, PluginSecretDelivery, take_plugin_secret_updates},
};
use orbit_types::plugin::{PluginGrant, PluginPermissions, PluginSecretUpdateStatus};
use serde_json::json;
use std::{path::Path, sync::Arc};

/// A spec declaring `refresh_token` (rotatable) and `api_key` (not), over
/// an in-memory store with the host's compare-and-swap.
fn rotation_spec(root: &Path, source: Arc<CasSource>) -> PluginBackendSpec {
    let mut spec = (*spec(
        root.join("backend.sh"),
        root,
        PluginPermissions::default(),
        &[PluginGrant::Unsandboxed],
    ))
    .clone();
    spec.sandbox = orbit_types::plugin::PluginSandbox::None;
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
    let script = r#"#!/bin/sh
cat >/dev/null
printf '{ "ok": true, "output": {}, "secret_updates": { "refresh_token": { "value": "%s", "expected_version": "v1" } } }\n' "$2"
"#;
    stub_backend(temp.path(), script);
    let spec = Arc::new(rotation_spec(temp.path(), Arc::clone(&source)));
    let ctx = context(temp.path());
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let racers: Vec<_> = ["token-from-a-3c", "token-from-b-4d"]
        .into_iter()
        .map(|value| {
            let spec = Arc::clone(&spec);
            let barrier = Arc::clone(&barrier);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut call_spec = (*spec).clone();
                call_spec.args.push(value.to_string());
                tool(Arc::new(call_spec), None)
                    .execute(&ctx, json!({}))
                    .expect("rotation response");
                let outcome = take_plugin_secret_updates();
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
