use crate::auto_tasks::loader::{auto_tasks_dir, collect_auto_tasks};
use std::fs;
use tempfile::tempdir;

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
