use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::routines::loader::{
    LOCAL_ROUTINES_SUBDIR, ROUTINES_DIR, RoutineCatalogLookup, RoutineSource, collect_routines,
    declared_routine_names,
};

fn resolving_catalog(_orbit_dir: &Path, _job: &str) -> RoutineCatalogLookup {
    RoutineCatalogLookup {
        resolves: true,
        error: None,
    }
}

fn collect(orbit_dir: &Path) -> crate::routines::loader::RoutineCollection {
    collect_routines(
        &[RoutineSource {
            workspace: "polaris".to_string(),
            orbit_dir: orbit_dir.to_path_buf(),
        }],
        &resolving_catalog,
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
fn loads_regular_yaml_files_and_ignores_other_entries() {
    let root = tempdir().expect("temporary Orbit root");
    let routines = root.path().join(ROUTINES_DIR);
    fs::create_dir_all(routines.join(LOCAL_ROUTINES_SUBDIR)).expect("routines directories");
    write_valid_routine(&routines, "valid");
    write_valid_routine(&routines.join(LOCAL_ROUTINES_SUBDIR), "local-valid");
    fs::write(routines.join("notes.txt"), "not a routine").expect("non-definition fixture");
    fs::create_dir(routines.join("nested.yaml")).expect("nested fixture");

    let collection = collect(root.path());

    let mut names: Vec<_> = collection
        .routines
        .iter()
        .map(|routine| routine.definition.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["local-valid", "valid"]);
    assert!(collection.errors.is_empty());
}

#[test]
fn missing_routines_directory_is_an_empty_source() {
    let root = tempdir().expect("temporary Orbit root");

    let collection = collect(root.path());

    assert!(collection.routines.is_empty());
    assert!(collection.errors.is_empty());
}

#[test]
fn declared_names_include_both_origins() {
    let root = tempdir().expect("temporary Orbit root");
    let routines = root.path().join(ROUTINES_DIR);
    fs::create_dir_all(routines.join(LOCAL_ROUTINES_SUBDIR)).expect("routines directories");
    write_valid_routine(&routines, "committed");
    write_valid_routine(&routines.join(LOCAL_ROUTINES_SUBDIR), "local");

    let declared = declared_routine_names(root.path());

    assert_eq!(declared.len(), 2);
    assert!(declared.contains_key("committed"));
    assert!(declared.contains_key("local"));
}

#[cfg(unix)]
#[test]
fn rejects_routines_directory_symlink() {
    use std::os::unix::fs::symlink;

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

#[cfg(unix)]
#[test]
fn rejects_local_routines_directory_symlink() {
    use std::os::unix::fs::symlink;

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

#[cfg(unix)]
#[test]
fn ignores_symlinked_definition_files() {
    use std::os::unix::fs::symlink;

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
