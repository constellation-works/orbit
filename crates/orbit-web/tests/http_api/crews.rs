//! `GET /api/crews`: the crew list served from the cached workspace runtime
//! must reflect `config.toml` edits without a dashboard restart.

use std::fs;

use serde_json::Value;

use super::support::{Fixture, isolated, json_ok};

const CREWS: &str = "/api/crews?workspace=ws_http_fixture";

fn crew_names(body: &Value) -> Vec<String> {
    body["crews"]
        .as_array()
        .expect("crews list")
        .iter()
        .map(|crew| crew["name"].as_str().expect("crew name").to_string())
        .collect()
}

fn append_crew(path: &std::path::Path, name: &str, as_default: bool) {
    let mut text = fs::read_to_string(path).unwrap_or_default();
    if as_default {
        text.push_str(&format!("\n[workflow]\ndefault_crew = \"{name}\"\n"));
    }
    text.push_str(&format!(
        "\n[crews.{name}]\nprovider = \"codex\"\nmodel = \"fixture-{name}-model\"\n"
    ));
    fs::write(path, text).unwrap();
}

#[test]
fn crews_list_reflects_config_edits_without_restart() {
    isolated(
        "crews::crews_list_reflects_config_edits_without_restart",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);

            let before = json_ok(server.get(CREWS));
            assert!(
                !crew_names(&before).contains(&"fixturefreshws".to_string()),
                "fixture crew must start absent: {before}"
            );

            // Workspace-layer edit: the next request must be served from a
            // runtime built from the new config, with no server restart.
            append_crew(&fixture.work.join("config.toml"), "fixturefreshws", true);
            let after_workspace = json_ok(server.get(CREWS));
            assert!(
                crew_names(&after_workspace).contains(&"fixturefreshws".to_string()),
                "crew added to workspace config.toml must appear without restart: {after_workspace}"
            );

            // Global-layer edit: same invalidation for the global config.toml.
            append_crew(
                &fixture.global.join("config.toml"),
                "fixturefreshglobal",
                false,
            );
            let after_global = json_ok(server.get(CREWS));
            let names = crew_names(&after_global);
            assert!(
                names.contains(&"fixturefreshglobal".to_string()),
                "crew added to global config.toml must appear without restart: {after_global}"
            );
            assert!(
                names.contains(&"fixturefreshws".to_string()),
                "workspace crew must survive the global edit: {after_global}"
            );
        },
    );
}
