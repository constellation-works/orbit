//! `[plugins.<ns>]`: schema validation at load, key admission, and the
//! section belonging to a plugin this host has not installed (design §1, §3).
//!
//! These tests write the workspace `config.toml` directly rather than going
//! through `orbit config set`: the question is what a config file already on
//! disk does to a runtime build, which is the case an operator hits after
//! pulling a colleague's workspace.

use orbit_types::plugin::PluginStatus;

use super::super::{PluginAddOptions, install_plugin, show_plugin};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;

fn install(fixture: &PluginFixture, plugin: &DefinitionPlugin<'_>) {
    let source = plugin.write(fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
}

fn write_config(fixture: &PluginFixture, body: &str) {
    std::fs::write(fixture.workspace_root.join("config.toml"), body).expect("write config");
}

#[test]
fn a_value_the_plugin_schema_rejects_refuses_that_plugin_and_names_the_key() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    write_config(&fixture, "[plugins.graph]\ndepth = \"deep\"\n");

    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "graph").expect("show");
    assert_eq!(summary.status, PluginStatus::Inactive);
    let diagnostic = summary.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("plugins.graph.depth"),
        "the diagnostic names the key: {diagnostic}"
    );
}

#[test]
fn a_configured_value_reaches_the_plugin_over_its_manifest_default() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    write_config(&fixture, "[plugins.graph]\nindex_dir = \".custom\"\n");

    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "graph").expect("show").status,
        PluginStatus::Active
    );

    let effective = orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))
    .expect("effective config");
    let row = effective
        .values()
        .iter()
        .find(|value| value.key == "plugins.graph.index_dir")
        .expect("a row for the configured key");
    assert_eq!(row.value, serde_json::json!(".custom"));
    assert_eq!(
        row.source.kind(),
        orbit_config::ConfigValueSourceKind::Workspace
    );
}

#[test]
fn an_undeclared_key_is_refused_with_the_keys_that_would_have_worked() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    // Opening the runtime publishes the installed plugins' contracts, which is
    // what lets admission tell a declared key from a typo.
    let _runtime = fixture.reopen();

    let error = orbit_config::admit_config_key("plugins.graph.undeclared")
        .expect_err("an undeclared key is refused");
    let orbit_common::OrbitError::InvalidInputDiagnostic {
        message,
        did_you_mean,
    } = &error
    else {
        panic!("a refusal carrying the keys that would have worked: {error}");
    };
    assert!(
        message.contains("declares no config key 'undeclared'"),
        "{message}"
    );
    assert!(
        did_you_mean.contains(&"plugins.graph.index_dir".to_string()),
        "{did_you_mean:?}"
    );
    orbit_config::admit_config_key("plugins.graph.index_dir").expect("a declared key is admitted");
}

#[test]
fn a_section_for_an_uninstalled_plugin_leaves_the_runtime_standing() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    write_config(
        &fixture,
        "[plugins.graph]\nindex_dir = \".custom\"\n\n[plugins.other]\nanything = true\n",
    );

    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "graph").expect("show").status,
        PluginStatus::Active,
        "an unknown section is a warning, not a failed build"
    );
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))
    .expect("the config still loads");
    assert!(config.plugins.contains_key("other"));
}
