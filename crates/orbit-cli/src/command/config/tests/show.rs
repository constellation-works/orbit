use std::fs;

use orbit_config::{ConfigRoots, load_effective_config};

use super::super::show::{effective_json, effective_text, scoped_json, scoped_text};
use super::super::support::{ConfigScopeArg, open_store_for_scope};
use super::test_runtime;

#[test]
fn effective_json_attributes_each_merged_value_to_its_source() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        r#"
[workflow]
base_branch = "integration"

[execution.codex]
sandbox = "danger-full-access"
"#,
    )
    .expect("write global config");
    fs::write(
        workspace_root.join("config.toml"),
        "[scoring]\nenabled = false\n",
    )
    .expect("write workspace config");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let json = effective_json(&runtime, effective.values());

    assert_eq!(json["settings"]["scoring.enabled"], false);
    assert_eq!(json["provenance"]["scoring.enabled"]["scope"], "workspace");
    assert_eq!(
        json["provenance"]["workflow.base_branch"]["scope"],
        "global"
    );
    assert_eq!(
        json["provenance"]["execution.codex.sandbox"]["scope"],
        "built-in"
    );
    assert_eq!(
        json["provenance"]["workflow.base_branch"]["path"],
        global_root.join("config.toml").to_string_lossy().as_ref()
    );
}

#[test]
fn effective_json_includes_configured_crew_effort_and_omits_unconfigured() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "gpt-5.6-sol"
provider = "codex"
effort = "medium"

[crews.opus]
model = "opus"
provider = "claude"
"#,
    )
    .expect("write global config");
    fs::write(
        workspace_root.join("config.toml"),
        "[crews.sol]\neffort = \"high\"\n",
    )
    .expect("write workspace override");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let json = effective_json(&runtime, effective.values());

    assert_eq!(json["settings"]["crews.sol.effort"], "high");
    assert_eq!(json["provenance"]["crews.sol.effort"]["scope"], "workspace");
    assert_eq!(
        json["provenance"]["crews.sol.effort"]["path"],
        workspace_root
            .join("config.toml")
            .to_string_lossy()
            .as_ref()
    );
    assert!(
        json["settings"].get("crews.opus.effort").is_none(),
        "omitted effort must not appear in settings: {}",
        json["settings"]
    );
}

#[test]
fn config_show_omits_the_retired_workspace_task_projection() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");

    let json = effective_json(&runtime, effective.values());
    let text = effective_text(&runtime, effective.values());
    let scoped_store =
        open_store_for_scope(&runtime, ConfigScopeArg::Global).expect("open global scoped config");
    let scoped_snapshot = scoped_store.snapshot().expect("read scoped config");
    let scoped_settings = scoped_snapshot.all_values();
    let scoped_json = scoped_json(&runtime, &scoped_store, &scoped_snapshot, &scoped_settings);
    let scoped_text = scoped_text(&runtime, &scoped_store, &scoped_snapshot, &scoped_settings);

    assert!(
        json["persistence"].get("task").is_none(),
        "configuration must not advertise the removed workspace task projection: {}",
        json["persistence"]
    );
    assert!(
        !text.contains("\"task\""),
        "text configuration output must not advertise the removed task store: {text}"
    );
    assert!(
        scoped_json["persistence"].get("task").is_none(),
        "scoped JSON must not advertise the removed task store: {}",
        scoped_json["persistence"]
    );
    assert!(
        !scoped_text.contains("\"task\""),
        "scoped text must not advertise the removed task store: {scoped_text}"
    );
}

#[test]
fn config_show_marks_absent_workspace_layer_and_lists_every_settable_key() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "[workflow]\nbase_branch = \"integration\"\n",
    )
    .expect("write global config");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective");
    let json = effective_json(&runtime, effective.values());
    let text = effective_text(&runtime, effective.values());

    // 1. Marks absent workspace layer
    assert_eq!(json["source"]["workspace_exists"], false);
    assert_eq!(json["source"]["global_exists"], true);
    assert!(text.contains("workspace:"));
    assert!(
        text.contains("(absent)"),
        "text must mark absent layer: {text}"
    );

    // 2. Lists every settable key from CONFIG_KEY_REGISTRY
    for descriptor in orbit_config::CONFIG_KEY_REGISTRY {
        assert!(
            json["settings"].get(descriptor.key).is_some(),
            "settings must contain {}",
            descriptor.key
        );
        assert!(
            text.contains(descriptor.key),
            "text must list {}",
            descriptor.key
        );
    }

    // 3. Unset keys render as [unset]
    assert!(
        text.contains("tasks.id_start"),
        "tasks.id_start must be present: {text}"
    );
    assert!(
        text.contains("[unset]"),
        "unset keys must display [unset]: {text}"
    );

    // 4. Scoped show for workspace also marks absent
    let scoped_store =
        open_store_for_scope(&runtime, ConfigScopeArg::Workspace).expect("open workspace store");
    let scoped_snapshot = scoped_store.snapshot().expect("snapshot");
    let scoped_settings = scoped_snapshot.all_values();
    let s_json = scoped_json(&runtime, &scoped_store, &scoped_snapshot, &scoped_settings);
    let s_text = scoped_text(&runtime, &scoped_store, &scoped_snapshot, &scoped_settings);

    assert_eq!(s_json["source"]["exists"], false);
    assert!(s_text.contains("(absent)"));
    assert!(s_text.contains("[unset]"));
}
