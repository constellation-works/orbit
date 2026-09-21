//! Seeding, re-seeding and what happens to a seeded schedule when its plugin
//! is disabled (design §3, §4.5).

use std::path::PathBuf;

use orbit_types::workspace::{Workspace, WorkspaceStatus};

use super::super::{PluginAddOptions, PluginEnableOptions, disable_plugin, enable_plugin};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::application::plugin::PluginSeedAction;

fn install(fixture: &PluginFixture, plugin: &DefinitionPlugin<'_>) {
    let source = plugin.write(fixture);
    super::super::install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            force: true,
            // Enabled explicitly by each test, so the seeding actions below
            // are the ones `orbit plugin enable` reports.
            enable: false,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
}

fn routine_path(fixture: &PluginFixture) -> PathBuf {
    fixture.workspace_root.join("routines/graph-refresh.yaml")
}

fn auto_task_path(fixture: &PluginFixture) -> PathBuf {
    fixture.workspace_root.join("auto_tasks/graph-reindex.yaml")
}

#[test]
fn enable_seeds_each_definition_disabled_and_stamped_with_its_plugin() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    let runtime = fixture.reopen();

    enable_plugin(&runtime, "graph", &PluginEnableOptions::default()).expect("enable");

    let routine = std::fs::read_to_string(routine_path(&fixture)).expect("seeded routine");
    assert!(
        routine.contains("# provenance: plugin:graph@1.0.0"),
        "{routine}"
    );
    assert!(routine.contains("name: graph-refresh"), "{routine}");
    assert!(routine.contains("enabled: false"), "{routine}");

    let auto_task = std::fs::read_to_string(auto_task_path(&fixture)).expect("seeded auto-task");
    assert!(
        auto_task.contains("# provenance: plugin:graph@1.0.0"),
        "{auto_task}"
    );
    assert!(auto_task.contains("enabled: false"), "{auto_task}");
}

#[test]
fn an_upgrade_reseeds_an_untouched_file_and_preserves_a_customised_one() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    let runtime = fixture.reopen();
    enable_plugin(&runtime, "graph", &PluginEnableOptions::default()).expect("first enable");

    // One file is left exactly as Orbit wrote it; the other is edited the way
    // an operator would to switch its schedule on.
    let customised = std::fs::read_to_string(auto_task_path(&fixture))
        .expect("seeded auto-task")
        .replace("enabled: false", "enabled: true");
    std::fs::write(auto_task_path(&fixture), &customised).expect("customise the auto-task");

    // Disabled across the upgrade, so the re-enable's own report is what the
    // assertions below read rather than the install's.
    disable_plugin(&runtime, "graph").expect("disable before the upgrade");
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_version("1.1.0"),
    );
    let runtime = fixture.reopen();
    let result =
        enable_plugin(&runtime, "graph", &PluginEnableOptions::default()).expect("re-enable");

    let routine = result
        .seeded
        .iter()
        .find(|seeded| seeded.kind == "routine")
        .expect("a routine row");
    assert_eq!(routine.action, PluginSeedAction::Refreshed);
    assert!(
        std::fs::read_to_string(routine_path(&fixture))
            .expect("routine")
            .contains("plugin:graph@1.1.0")
    );

    let auto_task = result
        .seeded
        .iter()
        .find(|seeded| seeded.kind == "auto_task")
        .expect("an auto-task row");
    assert_eq!(auto_task.action, PluginSeedAction::Customised);
    let warning = auto_task.warning.clone().expect("a warning");
    assert!(warning.contains("--force"), "{warning}");
    assert_eq!(
        std::fs::read_to_string(auto_task_path(&fixture)).expect("auto-task"),
        customised,
        "a customised file is left exactly as the operator wrote it"
    );

    let forced = enable_plugin(
        &runtime,
        "graph",
        &PluginEnableOptions {
            force: true,
            ..PluginEnableOptions::default()
        },
    )
    .expect("forced re-enable");
    assert_eq!(
        forced
            .seeded
            .iter()
            .find(|seeded| seeded.kind == "auto_task")
            .expect("an auto-task row")
            .action,
        PluginSeedAction::Refreshed
    );
    let reseeded = std::fs::read_to_string(auto_task_path(&fixture)).expect("auto-task");
    assert!(reseeded.contains("enabled: false"), "{reseeded}");
    assert!(reseeded.contains("plugin:graph@1.1.0"), "{reseeded}");
}

#[test]
fn a_disabled_plugins_seeded_schedules_are_skipped_with_a_reason_naming_it() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    let runtime = fixture.reopen();
    enable_plugin(&runtime, "graph", &PluginEnableOptions::default()).expect("enable");

    // While the plugin is enabled, its seeded auto-task is an ordinary
    // definition: disabled by seeding, but not skipped.
    let runtime = fixture.reopen();
    let definition = runtime
        .auto_task_show("graph-reindex")
        .expect("show")
        .expect("the seeded definition");
    assert_eq!(runtime.auto_task_skip_reason(&definition), None);

    disable_plugin(&runtime, "graph").expect("disable");
    let runtime = fixture.reopen();
    let reason = runtime
        .auto_task_skip_reason(&definition)
        .expect("a skip reason");
    assert!(
        reason.contains("plugin:graph@1.0.0") && reason.contains("orbit plugin enable graph"),
        "{reason}"
    );

    let collection =
        crate::application::routines::collect_routines(&[(workspace_record(), fixture.reopen())]);
    let retired = collection
        .retired
        .iter()
        .find(|routine| routine.name == "graph-refresh")
        .expect("the seeded routine is skipped, not a load error");
    assert!(
        retired.reason.contains("plugin:graph@1.0.0"),
        "{}",
        retired.reason
    );
    assert!(
        collection.errors.is_empty(),
        "a disabled plugin is not a load error: {:?}",
        collection.errors
    );
}

#[test]
fn a_task_minted_from_a_seeded_auto_task_carries_its_plugin_tag() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    let runtime = fixture.reopen();
    enable_plugin(&runtime, "graph", &PluginEnableOptions::default()).expect("enable");

    let runtime = fixture.reopen();
    let task = runtime.auto_task_mint("graph-reindex").expect("mint");
    assert!(
        task.tags.contains(&"plugin:graph".to_string()),
        "{:?}",
        task.tags
    );
    assert!(
        task.tags.contains(&"auto-task:graph-reindex".to_string()),
        "{:?}",
        task.tags
    );
}

/// The registry record routine discovery takes, for a fixture that has no
/// registry behind it.
fn workspace_record() -> Workspace {
    let now = chrono::Utc::now();
    Workspace {
        id: "ws_fixture".to_string(),
        name: "fixture".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    }
}
