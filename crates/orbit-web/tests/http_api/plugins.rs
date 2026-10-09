use std::fs;
use std::os::unix::fs::PermissionsExt;

use orbit_core::adapter::command::{PluginAddOptions, PluginEnableOptions, PluginSecretValue};
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok, write_json};

fn install(fixture: &Fixture, extra: &str) {
    let source = fixture.path("source/.orbit-plugin");
    fs::create_dir_all(source.join("bin")).unwrap();
    let backend = source.join("bin/backend.sh");
    fs::write(
        &backend,
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{}}\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(source.join("plugin.yaml"), format!(
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: http-fixture\n  version: 0.1.0\n  description: HTTP security fixture.\nspec:\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: status\n      description: Read status.\n      execution_kind: read_only\n      mcp_scope: workspace\n{extra}"
    )).unwrap();
    fixture
        .runtime
        .add_plugin(source.to_str().unwrap(), &PluginAddOptions::default())
        .unwrap();
}

#[test]
fn plugin_enable_requires_matching_recorded_grants_and_program_paths() {
    isolated(
        "plugins::plugin_enable_requires_matching_recorded_grants_and_program_paths",
        || {
            let fixture = Fixture::new();
            install(
                &fixture,
                "  permissions:\n    network: loopback\n  requires:\n    programs: [sh]\n",
            );
            let server = fixture.server(true);
            let enable = || {
                server.send(
                    "POST",
                    "/api/plugins/http-fixture/enable?workspace=ws_http_fixture",
                    json!({"scope":"host"}),
                )
            };
            error_code(enable(), 409, "plugin_refused");
            fixture
                .runtime
                .enable_plugin("http-fixture", &PluginEnableOptions::default())
                .unwrap();
            fixture.runtime.disable_plugin("http-fixture").unwrap();
            error_code(enable(), 409, "plugin_refused");
            let listed = json_ok(server.get("/api/plugins?workspace=ws_http_fixture"));
            assert_eq!(
                listed[0]["host_orbit_version"],
                env!("CARGO_PKG_VERSION"),
                "plugin certification is compared with the serving host version"
            );
            assert_eq!(
                listed[0]["host_enabled"], false,
                "HTTP cannot mint consent on behalf of an operator"
            );

            fixture
                .runtime
                .enable_plugin(
                    "http-fixture",
                    &PluginEnableOptions {
                        grants: vec!["network".into()],
                        force: false,
                    },
                )
                .unwrap();
            fixture.runtime.disable_plugin("http-fixture").unwrap();
            let witness = fixture.global.join("plugins/.grants/http-fixture.json");
            let mut recorded: Value = serde_json::from_slice(&fs::read(&witness).unwrap()).unwrap();
            recorded["programs"]["sh"] = json!("/changed/sh");
            write_json(&witness, recorded);
            error_code(enable(), 409, "plugin_refused");
            assert_eq!(
                json_ok(server.get("/api/plugins?workspace=ws_http_fixture"))[0]["host_enabled"],
                false
            );

            fixture
                .runtime
                .enable_plugin("http-fixture", &PluginEnableOptions::default())
                .unwrap();
            fixture.runtime.disable_plugin("http-fixture").unwrap();
            let enabled = json_ok(enable());
            assert_eq!(
                enabled["plugin"]["host_enabled"], true,
                "matching reviewed consent permits HTTP re-enable"
            );
        },
    );
}

#[test]
fn plugin_listings_never_expose_stored_secret_values() {
    isolated(
        "plugins::plugin_listings_never_expose_stored_secret_values",
        || {
            const SECRET: &str = "http-secret-marker-687cb9";
            let fixture = Fixture::new();
            install(
                &fixture,
                "  secrets:\n    - name: token\n      description: API token.\n",
            );
            fixture
                .runtime
                .enable_plugin("http-fixture", &PluginEnableOptions::default())
                .unwrap();
            fixture
                .runtime
                .set_plugin_secret(
                    "http-fixture",
                    "token",
                    &PluginSecretValue::new(SECRET.into()).unwrap(),
                )
                .unwrap();
            for operator in [false, true] {
                let server = fixture.server(operator);
                for enabled in [true, false] {
                    if !enabled {
                        fixture.runtime.disable_plugin("http-fixture").unwrap();
                    }
                    let response = server.get("/api/plugins?workspace=ws_http_fixture");
                    assert_eq!(response.status().as_u16(), 200);
                    let text = response.text().unwrap();
                    assert!(
                        !text.contains(SECRET),
                        "plugin HTTP listings must never expose a persisted secret"
                    );
                    let listed: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(listed[0]["name"], "http-fixture");
                    assert_eq!(listed[0]["host_enabled"], enabled);
                }
                fixture
                    .runtime
                    .enable_plugin("http-fixture", &PluginEnableOptions::default())
                    .unwrap();
            }
        },
    );
}
