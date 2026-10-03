//! CRUD-surface tests [ORB-10149]: add/list/show/update/toggle roundtrip,
//! duplicate rejection, fail-closed parsing, and the no-turn-knobs guarantee.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{AutoTaskSchedule, DedupePolicy, auto_task_tag};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::auto_tasks::crud::AutoTaskUpdateParams;

use super::{interval_params, template};

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

#[test]
fn add_list_show_roundtrip() {
    let runtime = runtime();
    let mut params = interval_params("nightly-chore", 1440);
    params.template.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    let created = runtime.auto_task_add(params).expect("add");
    assert_eq!(created.name, "nightly-chore");
    assert!(created.enabled);
    assert_eq!(created.created_by.as_deref(), Some(runtime.actor_label()));
    assert_eq!(
        created.template.required_tools,
        vec!["github.auth.status", "github.run.list"]
    );

    let listed = runtime.auto_task_list().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "nightly-chore");

    let shown = runtime.auto_task_show("nightly-chore").expect("show");
    assert_eq!(shown.expect("present").name, "nightly-chore");
    assert!(
        runtime
            .auto_task_show("missing")
            .expect("show missing")
            .is_none()
    );
}

#[test]
fn add_rejects_duplicate_name() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("dup", 60))
        .expect("add");
    let err = runtime
        .auto_task_add(interval_params("dup", 60))
        .expect_err("second add rejected");
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[test]
fn writes_reject_loader_invalid_definitions_without_repairing_them() {
    let runtime = runtime();
    let root = runtime.paths().local_dir.clone();
    runtime
        .auto_task_add(interval_params("valid", 60))
        .expect("seed valid definition");
    let valid_path = root.join("auto_tasks/valid.yaml");
    let original = fs::read(&valid_path).expect("read valid definition");
    let wrong_stem = root.join("auto_tasks/wrong-stem.yaml");
    fs::write(&wrong_stem, &original).expect("seed mismatched name");

    for result in [
        runtime.auto_task_update(
            "valid",
            AutoTaskUpdateParams {
                description: Some("would overwrite".into()),
                ..Default::default()
            },
        ),
        runtime.auto_task_toggle("valid", false),
        runtime.auto_task_add(interval_params("another", 60)),
    ] {
        let error = result.expect_err("invalid host definition must reject every write");
        assert!(error.to_string().contains("stem"), "{error}");
    }
    assert_eq!(fs::read(&valid_path).expect("valid YAML"), original);
    assert_eq!(fs::read(&wrong_stem).expect("invalid YAML"), original);
    assert!(!root.join("auto_tasks/another.yaml").exists());

    fs::write(&wrong_stem, "name: malformed\nunknown: field\n").expect("seed parser rejection");
    assert!(runtime.auto_task_toggle("valid", false).is_err());
    assert_eq!(fs::read(&valid_path).expect("valid YAML"), original);
}

#[test]
fn update_does_not_create_a_second_definition_file_from_a_yml_source() {
    let runtime = runtime();
    let root = runtime.paths().local_dir.clone();
    runtime
        .auto_task_add(interval_params("single", 60))
        .expect("seed definition");
    let yaml = root.join("auto_tasks/single.yaml");
    let yml = root.join("auto_tasks/single.yml");
    fs::rename(&yaml, &yml).expect("use loader-supported extension");
    assert!(
        runtime
            .auto_task_update(
                "single",
                AutoTaskUpdateParams {
                    description: Some("changed".into()),
                    ..Default::default()
                },
            )
            .expect_err("editing .yml must not create a duplicate .yaml")
            .to_string()
            .contains("canonical .yaml")
    );
    assert!(!yaml.exists());
    assert!(yml.exists());
}

#[test]
fn add_rejects_invalid_name_and_schedule() {
    let runtime = runtime();
    let mut bad_name = interval_params("placeholder", 60);
    bad_name.name = "Bad Name".to_string();
    assert!(runtime.auto_task_add(bad_name).is_err());

    let mut bad_cron = interval_params("bad-cron", 60);
    bad_cron.schedule = AutoTaskSchedule::Cron {
        cron: "nonsense".to_string(),
    };
    assert!(runtime.auto_task_add(bad_cron).is_err());
}

#[test]
fn update_patches_present_fields() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");

    let mut replacement = template("Renamed chore");
    replacement.complexity = Some(TaskComplexity::Low);
    replacement.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    let updated = runtime
        .auto_task_update(
            "chore",
            AutoTaskUpdateParams {
                waive_batch: None,
                description: Some("new body".to_string()),
                schedule: Some(AutoTaskSchedule::Cron {
                    cron: "0 9 * * *".to_string(),
                }),
                dedupe: Some(DedupePolicy::Always),
                template: Some(replacement),
                enabled: Some(false),
            },
        )
        .expect("update");
    assert!(!updated.enabled, "`enabled` is an ordinary update field");
    assert_eq!(updated.description, "new body");
    assert_eq!(updated.dedupe, DedupePolicy::Always);
    assert_eq!(updated.template.title, "Renamed chore");
    assert_eq!(updated.template.complexity, Some(TaskComplexity::Low));
    assert_eq!(
        updated.template.required_tools,
        vec!["github.auth.status", "github.run.list"]
    );
    assert!(matches!(updated.schedule, AutoTaskSchedule::Cron { .. }));
    assert!(updated.updated_at >= updated.created_at);
}

#[test]
fn template_updates_only_change_required_tools_for_future_tasks() {
    let runtime = runtime();
    let mut params = interval_params("authority", 60);
    params.template.required_tools = vec!["github.run.list".to_string()];
    runtime.auto_task_add(params).expect("add");
    let first = runtime.auto_task_mint("authority").expect("first mint");

    let mut replacement = template("Updated authority");
    replacement.required_tools = vec!["github.auth.status".to_string()];
    runtime
        .auto_task_update(
            "authority",
            AutoTaskUpdateParams {
                template: Some(replacement),
                ..Default::default()
            },
        )
        .expect("update template");
    let second = runtime.auto_task_mint("authority").expect("second mint");

    assert_eq!(
        runtime
            .get_task(&first.id)
            .expect("read first minted task")
            .required_tools,
        vec!["github.run.list"]
    );
    assert_eq!(second.required_tools, vec!["github.auth.status"]);
}

#[test]
fn toggle_disables_without_deleting() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");

    let disabled = runtime
        .auto_task_toggle("chore", false)
        .expect("toggle off");
    assert!(!disabled.enabled);
    assert!(runtime.auto_task_show("chore").expect("show").is_some());

    let enabled = runtime.auto_task_toggle("chore", true).expect("toggle on");
    assert!(enabled.enabled);
}

/// A workspace reached through a symlinked ancestor (macOS `/tmp` and `/var`,
/// a symlinked `~/workspace`) is an ordinary setup. Discovery lists the
/// resolved directory, so an edit must recognise its own definition file
/// rather than refuse it as stored at a non-canonical path.
#[cfg(unix)]
#[test]
fn edits_work_when_the_workspace_is_reached_through_a_symlinked_ancestor() {
    let temp = tempdir().expect("tempdir");
    let real = temp.path().join("real");
    fs::create_dir_all(real.join("global")).expect("global root");
    fs::create_dir_all(real.join("repo/.orbit")).expect("orbit dir");
    let linked = temp.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("link the workspace parent");

    let runtime = OrbitRuntime::from_roots(&linked.join("global"), &linked.join("repo/.orbit"))
        .expect("runtime behind a symlink");
    assert!(
        runtime.paths().local_dir.starts_with(&linked),
        "the fixture must keep the symlinked spelling: {}",
        runtime.paths().local_dir.display()
    );
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");

    let disabled = runtime
        .auto_task_toggle("chore", false)
        .expect("toggle off");
    assert!(!disabled.enabled);
    runtime
        .auto_task_update("chore", AutoTaskUpdateParams::default())
        .expect("update");
    assert!(
        runtime
            .auto_task_toggle("chore", true)
            .expect("toggle on")
            .enabled
    );
}

#[test]
fn update_missing_definition_errors() {
    let runtime = runtime();
    assert!(runtime.auto_task_toggle("ghost", false).is_err());
    assert!(
        runtime
            .auto_task_update("ghost", AutoTaskUpdateParams::default())
            .is_err()
    );
}

#[test]
fn parse_rejects_turn_based_knobs_and_unknown_fields() {
    // ADR-0217: the schema is provider-neutral; a turn budget anywhere in the
    // definition (including the template) is a hard parse error.
    let with_turns = r#"
schemaVersion: 1
name: chore
schedule:
  every_minutes: 60
template:
  title: Chore
  max_turns: 40
"#;
    assert!(parse_auto_task_yaml(with_turns).is_err());

    let top_level_turns = r#"
schemaVersion: 1
name: chore
turns: 10
schedule:
  every_minutes: 60
template:
  title: Chore
"#;
    assert!(parse_auto_task_yaml(top_level_turns).is_err());
}

#[test]
fn linked_worktree_refresh_is_atomic_and_never_mutates_primary_definition() {
    let root = tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let primary_orbit = root.path().join("primary/.orbit");
    let worktree_orbit = root.path().join("worktree/.orbit");
    for path in [&global_root, &primary_orbit, &worktree_orbit] {
        fs::create_dir_all(path).expect("runtime root");
    }
    let runtime = OrbitRuntime::from_resolved_roots(&global_root, &primary_orbit, &worktree_orbit)
        .expect("two-root runtime");

    runtime
        .auto_task_add(interval_params("doc-duties", 60))
        .expect("seed worktree definition");
    let worktree_path = worktree_orbit.join("auto_tasks/doc-duties.yaml");
    let primary_path = primary_orbit.join("auto_tasks/doc-duties.yaml");
    fs::create_dir_all(primary_path.parent().expect("primary parent")).expect("primary parent");
    fs::copy(&worktree_path, &primary_path).expect("seed primary definition");
    let primary_before = fs::read(&primary_path).expect("primary before");

    runtime
        .auto_task_update(
            "doc-duties",
            AutoTaskUpdateParams {
                description: Some("refreshed in the assigned worktree".to_string()),
                ..Default::default()
            },
        )
        .expect("refresh");

    assert_eq!(
        fs::read(&primary_path).expect("primary after"),
        primary_before,
        "tracked primary definition must stay byte-identical"
    );
    let refreshed = fs::read_to_string(&worktree_path).expect("worktree definition");
    assert!(refreshed.contains("refreshed in the assigned worktree"));
    assert!(
        fs::read_dir(worktree_path.parent().expect("worktree parent"))
            .expect("list worktree auto_tasks")
            .all(|entry| {
                !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            }),
        "atomic replacement must not leave staging files"
    );
}

#[test]
fn failed_linked_worktree_refresh_preserves_primary_and_names_definition() {
    let root = tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let primary_orbit = root.path().join("primary/.orbit");
    let worktree_orbit = root.path().join("worktree/.orbit");
    for path in [&global_root, &primary_orbit, &worktree_orbit] {
        fs::create_dir_all(path).expect("runtime root");
    }

    let primary_path = primary_orbit.join("auto_tasks/doc-duties.yaml");
    fs::create_dir_all(primary_path.parent().expect("primary parent")).expect("primary parent");
    fs::write(&primary_path, "primary-definition-bytes\n").expect("primary definition");
    let primary_before = fs::read(&primary_path).expect("primary before");

    // A non-directory local `auto_tasks` path makes the refresh fail before a
    // staged file can be committed. This models a filesystem failure without
    // relying on platform-specific permission behavior.
    fs::write(worktree_orbit.join("auto_tasks"), "not a directory").expect("blocking path");
    let runtime = OrbitRuntime::from_resolved_roots(&global_root, &primary_orbit, &worktree_orbit)
        .expect("two-root runtime");
    let error = runtime
        .auto_task_add(interval_params("doc-duties", 60))
        .expect_err("refresh must fail");

    assert!(
        error.to_string().contains("doc-duties"),
        "durable tool error must identify the auto-task: {error}"
    );
    assert_eq!(
        fs::read(&primary_path).expect("primary after"),
        primary_before,
        "failed refresh must leave primary byte-identical"
    );
}

fn write_external_definition(directory: &Path, name: &str) {
    fs::create_dir_all(directory).expect("definition directory");
    fs::write(
        directory.join(format!("{name}.yaml")),
        format!(
            r#"schemaVersion: 1
name: {name}
description: outside the definition directory
schedule:
  every_minutes: 60
template:
  title: Outside {name}
"#
        ),
    )
    .expect("write external definition");
}

fn assert_lookup_refused(runtime: &OrbitRuntime, name: &str) {
    let shown = runtime.auto_task_show(name);
    assert!(
        matches!(shown, Err(OrbitError::InvalidInput(_))),
        "show {name:?} must refuse the lookup before accepting it, got {shown:?}"
    );
    let minted = runtime.auto_task_mint(name);
    assert!(
        matches!(minted, Err(OrbitError::InvalidInput(_))),
        "mint {name:?} must refuse the lookup before creating a task, got {minted:?}"
    );
}

fn task_count(runtime: &OrbitRuntime) -> usize {
    runtime.list_tasks().expect("list tasks").len()
}

#[test]
fn show_and_mint_reject_absolute_and_traversal_lookups() {
    let runtime = runtime();
    let orbit = runtime.paths().local_dir.clone();
    write_external_definition(&orbit, "outside");
    let absolute_root = tempdir().expect("absolute definition root");
    write_external_definition(absolute_root.path(), "escaped");
    let absolute_name = absolute_root
        .path()
        .join("escaped")
        .to_str()
        .expect("utf-8 absolute path")
        .to_string();

    for name in [
        "../outside",
        "..",
        "foo/../../outside",
        absolute_name.as_str(),
    ] {
        assert_lookup_refused(&runtime, name);
    }
    assert_eq!(task_count(&runtime), 0);

    let params = interval_params("chore", 60);
    assert_eq!(params.dedupe, DedupePolicy::SkipIfOpen);
    runtime
        .auto_task_add(params)
        .expect("add in-scope definition");
    runtime
        .auto_task_toggle("chore", false)
        .expect("disable definition");
    let shown = runtime
        .auto_task_show("chore")
        .expect("show regular definition")
        .expect("regular definition is present");
    assert_eq!(shown.name, "chore");
    assert!(!shown.enabled);

    let first = runtime
        .auto_task_mint("chore")
        .expect("mint disabled definition");
    let second = runtime
        .auto_task_mint("chore")
        .expect("mint again while an instance is open");
    assert_ne!(first.id, second.id);
    assert!(first.tags.contains(&auto_task_tag("chore")));
    assert_eq!(task_count(&runtime), 2);

    assert_lookup_refused(&runtime, "../outside");
    assert_lookup_refused(&runtime, &absolute_name);
    assert_eq!(task_count(&runtime), 2);
}

#[cfg(unix)]
#[test]
fn show_and_mint_refuse_symlinked_definition_file() {
    use std::os::unix::fs::symlink;

    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("local", 60))
        .expect("add regular definition");
    let outside = tempdir().expect("symlink target root");
    write_external_definition(outside.path(), "linked");
    symlink(
        outside.path().join("linked.yaml"),
        runtime.paths().local_dir.join("auto_tasks/linked.yaml"),
    )
    .expect("definition symlink");

    let shown = runtime
        .auto_task_show("local")
        .expect("show regular sibling")
        .expect("regular sibling is present");
    assert_eq!(shown.name, "local");
    assert_lookup_refused(&runtime, "linked");
    runtime
        .auto_task_mint("local")
        .expect("mint regular sibling");
    assert_eq!(task_count(&runtime), 1);
    assert_lookup_refused(&runtime, "linked");
    assert_eq!(task_count(&runtime), 1);
}

#[cfg(unix)]
#[test]
fn show_and_mint_refuse_symlinked_auto_tasks_directory() {
    use std::os::unix::fs::symlink;

    let runtime = runtime();
    let outside = tempdir().expect("outside definitions root");
    let outside_definitions = outside.path().join("auto_tasks");
    write_external_definition(&outside_definitions, "escaped");
    let definitions = runtime.paths().local_dir.join("auto_tasks");
    match fs::symlink_metadata(&definitions) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.file_type().is_file() => {
            fs::remove_file(&definitions).expect("clear auto_tasks path");
        }
        Ok(_) => fs::remove_dir_all(&definitions).expect("clear auto_tasks directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("inspect auto_tasks before symlink fixture: {error}"),
    }
    symlink(&outside_definitions, &definitions).expect("directory symlink");

    assert_lookup_refused(&runtime, "escaped");
    let missing = runtime.auto_task_show("missing");
    assert!(
        matches!(missing, Err(OrbitError::InvalidInput(_))),
        "a symlinked auto_tasks directory is refused before the lookup reads it, got {missing:?}"
    );
    assert_eq!(task_count(&runtime), 0);
}
