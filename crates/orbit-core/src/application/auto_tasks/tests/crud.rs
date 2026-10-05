//! Definition lookups stay confined to the workspace's auto-task directory.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{DedupePolicy, auto_task_tag};
use tempfile::tempdir;

use crate::OrbitRuntime;

use super::interval_params;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
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
