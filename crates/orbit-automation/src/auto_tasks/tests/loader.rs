use std::fs;

use tempfile::tempdir;

use crate::auto_tasks::loader::{auto_tasks_dir, collect_auto_tasks};

fn write_valid_definition(definitions: &std::path::Path, name: &str) {
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
fn loads_regular_yaml_files_and_ignores_other_entries() {
    let root = tempdir().expect("temporary definition root");
    let definitions = auto_tasks_dir(root.path());
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    write_valid_definition(&definitions, "valid");
    fs::write(definitions.join("notes.txt"), "not an auto-task").expect("non-definition fixture");
    fs::create_dir(definitions.join("nested.yaml")).expect("nested fixture");

    let collection = collect_auto_tasks(root.path());

    assert_eq!(collection.definitions.len(), 1);
    assert_eq!(collection.definitions[0].definition.name, "valid");
    assert!(collection.errors.is_empty());
}

#[test]
fn rejects_definition_with_invalid_cron_during_collection() {
    let root = tempdir().expect("temporary definition root");
    let definitions = auto_tasks_dir(root.path());
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    fs::write(
        definitions.join("invalid-cron.yaml"),
        r#"schemaVersion: 1
name: invalid-cron
schedule:
  cron: "every day"
template:
  title: Invalid cron fixture
"#,
    )
    .expect("definition fixture");

    let collection = collect_auto_tasks(root.path());

    assert!(collection.definitions.is_empty());
    assert_eq!(collection.errors.len(), 1);
    assert!(
        collection.errors[0]
            .message
            .contains("invalid cron expression 'every day'")
    );
}

#[cfg(unix)]
#[test]
fn rejects_auto_tasks_directory_symlink() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("temporary Orbit root");
    let outside = tempdir().expect("outside directory");
    let outside_definitions = outside.path().join("auto_tasks");
    fs::create_dir_all(&outside_definitions).expect("outside definitions directory");
    write_valid_definition(&outside_definitions, "escaped");
    symlink(&outside_definitions, auto_tasks_dir(root.path())).expect("directory symlink");

    let collection = collect_auto_tasks(root.path());

    assert!(collection.definitions.is_empty());
    assert_eq!(collection.errors.len(), 1);
    assert!(collection.errors[0].message.contains("directly under"));
}

#[cfg(unix)]
#[test]
fn ignores_symlinked_definition_files() {
    use std::os::unix::fs::symlink;

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
