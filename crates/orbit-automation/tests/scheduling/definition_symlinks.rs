//! Definition discovery never follows a symlink out of the Orbit directory:
//! a linked definitions directory is refused and a linked definition file is
//! ignored, for auto-tasks and routines alike.
#![cfg(unix)]

use orbit_automation::auto_tasks::loader::{auto_tasks_dir, collect_auto_tasks};
use orbit_automation::routines::loader::{
    LOCAL_ROUTINES_SUBDIR, ROUTINES_DIR, RoutineCatalogLookup, RoutineCollection, RoutineSource,
    collect_routines,
};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use tempfile::tempdir;

fn write_valid_definition(definitions: &Path, name: &str) {
    fs::write(
        definitions.join(format!("{name}.yaml")),
        format!(
            r#"schemaVersion: 1
name: {name}
schedule:
  every_minutes: 60
template:
  title: Fixture {name}
"#
        ),
    )
    .expect("definition fixture");
}

#[test]
fn rejects_auto_tasks_directory_symlink() {
    let outside = tempdir().expect("outside directory");
    let outside_definitions = outside.path().join("auto_tasks");
    fs::create_dir_all(&outside_definitions).expect("outside definitions directory");
    write_valid_definition(&outside_definitions, "escaped");
    for target in [outside_definitions, outside.path().join("missing")] {
        let root = tempdir().expect("temporary Orbit root");
        symlink(&target, auto_tasks_dir(root.path())).expect("directory symlink");

        let collection = collect_auto_tasks(root.path());

        assert!(collection.definitions.is_empty());
        assert_eq!(
            collection.errors.len(),
            1,
            "a linked catalog must fail closed even when its target is missing"
        );
        assert!(collection.errors[0].message.contains("directly under"));
    }
}

#[test]
fn ignores_symlinked_auto_task_definition_files() {
    let root = tempdir().expect("temporary Orbit root");
    let outside = tempdir().expect("outside directory");
    let definitions = auto_tasks_dir(root.path());
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    let outside_definition = outside.path().join("linked.yaml");
    write_valid_definition(outside.path(), "linked");
    symlink(&outside_definition, definitions.join("linked.yaml")).expect("definition symlink");
    write_valid_definition(&definitions, "local");

    let collection = collect_auto_tasks(root.path());

    assert_eq!(collection.definitions.len(), 1);
    assert_eq!(collection.definitions[0].definition.name, "local");
    assert!(collection.errors.is_empty());
}

fn collect(orbit_dir: &Path) -> RoutineCollection {
    collect_routines(
        &[RoutineSource {
            workspace: "polaris".to_string(),
            orbit_dir: orbit_dir.to_path_buf(),
        }],
        &|_, _| RoutineCatalogLookup {
            resolves: true,
            error: None,
        },
    )
}

fn write_valid_routine(dir: &Path, name: &str) {
    fs::write(
        dir.join(format!("{name}.yaml")),
        format!(
            "schemaVersion: 1\nname: {name}\n\
             trigger: {{ cron: \"* * * * *\" }}\ntarget: job:noop\n"
        ),
    )
    .expect("routine fixture");
}

#[test]
fn rejects_routines_directory_symlink() {
    let root = tempdir().expect("temporary Orbit root");
    let outside = tempdir().expect("outside directory");
    let outside_routines = outside.path().join(ROUTINES_DIR);
    fs::create_dir_all(&outside_routines).expect("outside routines directory");
    write_valid_routine(&outside_routines, "escaped");
    symlink(&outside_routines, root.path().join(ROUTINES_DIR)).expect("directory symlink");

    let collection = collect(root.path());

    assert!(collection.routines.is_empty());
    assert_eq!(collection.errors.len(), 1);
    assert!(collection.errors[0].message.contains("directly under"));
}

#[test]
fn rejects_local_routines_directory_symlink() {
    let root = tempdir().expect("temporary Orbit root");
    let outside = tempdir().expect("outside directory");
    let routines = root.path().join(ROUTINES_DIR);
    fs::create_dir_all(&routines).expect("routines directory");
    let outside_local = outside.path().join("local");
    fs::create_dir_all(&outside_local).expect("outside local directory");
    write_valid_routine(&outside_local, "escaped");
    symlink(&outside_local, routines.join(LOCAL_ROUTINES_SUBDIR)).expect("directory symlink");
    write_valid_routine(&routines, "committed");

    let collection = collect(root.path());

    assert_eq!(collection.routines.len(), 1);
    assert_eq!(collection.routines[0].definition.name, "committed");
    assert_eq!(collection.errors.len(), 1);
    assert!(collection.errors[0].message.contains("directly under"));
}

#[test]
fn ignores_symlinked_routine_definition_files() {
    let root = tempdir().expect("temporary Orbit root");
    let outside = tempdir().expect("outside directory");
    let routines = root.path().join(ROUTINES_DIR);
    fs::create_dir_all(&routines).expect("routines directory");
    write_valid_routine(outside.path(), "linked");
    symlink(
        outside.path().join("linked.yaml"),
        routines.join("linked.yaml"),
    )
    .expect("definition symlink");
    write_valid_routine(&routines, "local");

    let collection = collect(root.path());

    assert_eq!(collection.routines.len(), 1);
    assert_eq!(collection.routines[0].definition.name, "local");
    assert!(collection.errors.is_empty());
}
