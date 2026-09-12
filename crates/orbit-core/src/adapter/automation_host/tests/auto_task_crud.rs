//! CRUD-surface tests [ORB-10149]: add/list/show/update/toggle roundtrip,
//! duplicate rejection, fail-closed parsing, and the no-turn-knobs guarantee.

use std::fs;

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::workflow::{AutoTaskSchedule, DedupePolicy};
use tempfile::tempdir;

use crate::OrbitRuntime;
use orbit_automation::auto_tasks::crud::AutoTaskUpdateParams;

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
            },
        )
        .expect("update");
    assert_eq!(updated.description, "new body");
    assert_eq!(updated.dedupe, DedupePolicy::Always);
    assert_eq!(updated.template.title, "Renamed chore");
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
