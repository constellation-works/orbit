//! Seeding, re-seeding and what happens to a seeded schedule when its plugin
//! is disabled (design §3, §4.5).

use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;

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

fn managed_manifest_path(fixture: &PluginFixture, directory: &str) -> PathBuf {
    fixture
        .workspace_root
        .join(directory)
        .join(".orbit-managed-plugin-assets.json")
}

#[cfg(unix)]
#[test]
fn redirected_seed_destinations_are_refused_before_either_catalog_changes() {
    if std::env::var_os("ORBIT_TEST_REDIRECTED_SEED_CHILD").is_some() {
        run_redirected_seed_cases();
        return;
    }
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .arg("redirected_seed_destinations_are_refused_before_either_catalog_changes")
        .arg("--nocapture")
        .env("ORBIT_TEST_REDIRECTED_SEED_CHILD", "1")
        .env_remove("ORBIT_WORKTREE_ROOT")
        .output()
        .expect("run isolated plugin seeding fixture");
    assert!(
        output.status.success(),
        "isolated fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn run_redirected_seed_cases() {
    use std::os::unix::fs::symlink;

    for force in [false, true] {
        for catalog in ["routines", "auto_tasks"] {
            for target in [
                "definition-existing",
                "definition-dangling",
                "manifest",
                "directory",
            ] {
                let fixture = PluginFixture::new();
                install(&fixture, &DefinitionPlugin::new("graph"));
                let runtime = fixture.reopen();
                let directory = fixture.workspace_root.join(catalog);
                let file = if catalog == "routines" {
                    routine_path(&fixture)
                } else {
                    auto_task_path(&fixture)
                };
                let external = fixture._root.path().join("external");
                std::fs::create_dir_all(&external).expect("create external directory");
                let external_file = external.join("sentinel");
                let initial = match target {
                    "definition-dangling" => None,
                    "manifest" => Some("{\"schemaVersion\":1,\"assets\":{}}"),
                    _ => Some("external sentinel"),
                };
                if let Some(initial) = initial {
                    std::fs::write(&external_file, initial).expect("write external sentinel");
                }
                match target {
                    "definition-existing" | "definition-dangling" => {
                        std::fs::create_dir_all(&directory).expect("create catalog directory");
                        symlink(&external_file, &file).expect("link definition");
                    }
                    "manifest" => {
                        std::fs::create_dir_all(&directory).expect("create catalog directory");
                        symlink(&external_file, managed_manifest_path(&fixture, catalog))
                            .expect("link manifest");
                    }
                    "directory" => {
                        if directory.exists() {
                            std::fs::rename(
                                &directory,
                                fixture.workspace_root.join(format!("{catalog}-held")),
                            )
                            .expect("move original catalog");
                        }
                        symlink(&external, &directory).expect("link catalog directory");
                    }
                    _ => unreachable!(),
                }
                let other_catalog = if catalog == "routines" {
                    "auto_tasks"
                } else {
                    "routines"
                };
                let other_file = if catalog == "routines" {
                    auto_task_path(&fixture)
                } else {
                    routine_path(&fixture)
                };
                let other_manifest = managed_manifest_path(&fixture, other_catalog);
                let target_manifest = managed_manifest_path(&fixture, catalog);
                let before_file = std::fs::read(&other_file).ok();
                let before_manifest = std::fs::read(&other_manifest).ok();
                let before_target_manifest = std::fs::read(&target_manifest).ok();
                let error = enable_plugin(
                    &runtime,
                    "graph",
                    &PluginEnableOptions {
                        force,
                        ..PluginEnableOptions::default()
                    },
                )
                .expect_err("redirected seed destination must be refused");
                assert!(
                    error.to_string().contains("symlink"),
                    "{catalog} {target} force={force}: {error}"
                );
                assert_eq!(
                    std::fs::read_to_string(&external_file).ok().as_deref(),
                    initial,
                    "external target changed: {catalog} {target} force={force}"
                );
                assert_eq!(
                    std::fs::read(&other_file).ok(),
                    before_file,
                    "other catalog was seeded: {catalog} {target} force={force}"
                );
                assert_eq!(
                    std::fs::read(&other_manifest).ok(),
                    before_manifest,
                    "other provenance changed: {catalog} {target} force={force}"
                );
                assert_eq!(
                    std::fs::read(&target_manifest).ok(),
                    before_target_manifest,
                    "refused catalog provenance changed: {catalog} {target} force={force}"
                );
                if target == "directory" {
                    assert!(
                        !external
                            .join(file.file_name().expect("seed file name"))
                            .exists(),
                        "external catalog was seeded"
                    );
                    assert!(
                        !external.join(".orbit-managed-plugin-assets.json").exists(),
                        "external provenance was written"
                    );
                }
            }
        }
    }

    // A link to bytes Orbit previously recorded must also be refused on an
    // upgrade, when the ordinary managed-file rule would refresh them.
    for force in [false, true] {
        for catalog in ["routines", "auto_tasks"] {
            let fixture = PluginFixture::new();
            install(&fixture, &DefinitionPlugin::new("graph"));
            let runtime = fixture.reopen();
            enable_plugin(&runtime, "graph", &PluginEnableOptions::default())
                .expect("seed original version");
            disable_plugin(&runtime, "graph").expect("disable before upgrade");
            install(
                &fixture,
                &DefinitionPlugin::new("graph").with_version("1.1.0"),
            );
            let runtime = fixture.reopen();
            let file = if catalog == "routines" {
                routine_path(&fixture)
            } else {
                auto_task_path(&fixture)
            };
            let external = fixture._root.path().join("old-managed-bytes");
            let old_bytes = std::fs::read(&file).expect("original managed bytes");
            std::fs::write(&external, &old_bytes).expect("copy managed bytes outside catalog");
            std::fs::remove_file(&file).expect("remove contained copy");
            symlink(&external, &file).expect("redirect managed definition");
            let routine_manifest = managed_manifest_path(&fixture, "routines");
            let auto_task_manifest = managed_manifest_path(&fixture, "auto_tasks");
            let before_routine_manifest =
                std::fs::read(&routine_manifest).expect("routine provenance");
            let before_auto_task_manifest =
                std::fs::read(&auto_task_manifest).expect("auto-task provenance");
            let other_file = if catalog == "routines" {
                auto_task_path(&fixture)
            } else {
                routine_path(&fixture)
            };
            let other_bytes = std::fs::read(&other_file).expect("other managed definition");
            let error = enable_plugin(
                &runtime,
                "graph",
                &PluginEnableOptions {
                    force,
                    ..PluginEnableOptions::default()
                },
            )
            .expect_err("managed bytes through a symlink must be refused");
            assert!(
                error.to_string().contains("symlink"),
                "{catalog} force={force}: {error}"
            );
            assert_eq!(std::fs::read(&external).expect("external bytes"), old_bytes);
            assert_eq!(
                std::fs::read(&other_file).expect("other bytes"),
                other_bytes
            );
            assert_eq!(
                std::fs::read(&routine_manifest).expect("routine provenance"),
                before_routine_manifest
            );
            assert_eq!(
                std::fs::read(&auto_task_manifest).expect("auto-task provenance"),
                before_auto_task_manifest
            );
        }
    }
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
fn ambiguous_routine_filename_is_refused_without_changing_the_first_owner() {
    let fixture = PluginFixture::new();
    let mut first = DefinitionPlugin::new("a");
    first.routine = "b-c";
    install(&fixture, &first);
    let runtime = fixture.reopen();
    enable_plugin(&runtime, "a", &PluginEnableOptions::default()).expect("enable first owner");

    let path = fixture.workspace_root.join("routines/a-b-c.yaml");
    let manifest_path = managed_manifest_path(&fixture, "routines");
    let first_contents = std::fs::read_to_string(&path).expect("first owner's routine");
    let first_manifest = std::fs::read_to_string(&manifest_path).expect("first manifest");

    let mut colliding = DefinitionPlugin::new("a-b");
    colliding.routine = "c";
    install(&fixture, &colliding);
    let runtime = fixture.reopen();
    let error = enable_plugin(
        &runtime,
        "a-b",
        &PluginEnableOptions {
            force: true,
            ..PluginEnableOptions::default()
        },
    )
    .expect_err("force must not take another plugin's managed routine");
    let message = error.to_string();
    assert!(
        message.contains("plugin 'a-b'")
            && message.contains("plugin 'a'")
            && message.contains("a-b-c.yaml")
            && message.contains("owned"),
        "the refusal names both plugins, the file and the ownership rule: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("preserved routine"),
        first_contents,
        "the a/b-c vs a-b/c collision must not overwrite the first file"
    );
    assert_eq!(
        std::fs::read_to_string(&manifest_path).expect("preserved manifest"),
        first_manifest,
        "the collision must not relabel the first plugin's manifest record"
    );
}

#[test]
fn ambiguous_auto_task_filename_is_refused_even_without_force() {
    let fixture = PluginFixture::new();
    let mut first = DefinitionPlugin::new("alpha-beta");
    first.auto_task = "gamma";
    install(&fixture, &first);
    let runtime = fixture.reopen();
    enable_plugin(&runtime, "alpha-beta", &PluginEnableOptions::default())
        .expect("enable first owner");

    let path = fixture
        .workspace_root
        .join("auto_tasks/alpha-beta-gamma.yaml");
    let manifest_path = managed_manifest_path(&fixture, "auto_tasks");
    let first_contents = std::fs::read_to_string(&path).expect("first owner's auto-task");
    let first_manifest = std::fs::read_to_string(&manifest_path).expect("first manifest");

    let mut colliding = DefinitionPlugin::new("alpha");
    colliding.auto_task = "beta-gamma";
    install(&fixture, &colliding);
    let runtime = fixture.reopen();
    let error = enable_plugin(&runtime, "alpha", &PluginEnableOptions::default())
        .expect_err("a later plugin must not take the managed auto-task");
    let message = error.to_string();
    assert!(
        message.contains("plugin 'alpha'")
            && message.contains("plugin 'alpha-beta'")
            && message.contains("alpha-beta-gamma.yaml"),
        "the refusal names both plugins and the colliding file: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("preserved auto-task"),
        first_contents
    );
    assert_eq!(
        std::fs::read_to_string(&manifest_path).expect("preserved manifest"),
        first_manifest
    );
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
