//! A sentence that starts with "Verify" is not the closing verify command.

use orbit_types::task::TaskStatus;

use super::enter_isolated_child;
use crate::application::task::{TaskAddParams, TaskWriteScreen};

fn runtime_with_operator() -> (tempfile::TempDir, crate::OrbitRuntime) {
    super::assert_isolated_child();
    let root = tempfile::tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo").join(".orbit");
    std::fs::create_dir_all(&global_root).expect("global");
    std::fs::create_dir_all(&workspace_root).expect("workspace");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[workflow]
default_crew = "implementer"

[crews.implementer]
model = "implementer-model"
provider = "codex"
backend = "cli"

[crews.operator]
kind = "human"
description = "Operator work"
"#,
    )
    .expect("config");
    let runtime = crate::OrbitRuntime::from_roots(&global_root, &workspace_root).expect("runtime");
    (root, runtime)
}

fn file(runtime: &crate::OrbitRuntime, description: &str) -> Vec<String> {
    runtime
        .add_task_screened(
            TaskAddParams {
                title: "Operator task".into(),
                description: description.into(),
                acceptance_criteria: vec!["The operator can run it.".into()],
                crew: Some("operator".into()),
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            },
            &TaskWriteScreen::unbound(true),
            None,
            None,
        )
        .expect("file")
        .warnings
}

#[test]
fn a_verify_sentence_does_not_satisfy_the_verify_command() {
    if !enter_isolated_child(
        module_path!(),
        "a_verify_sentence_does_not_satisfy_the_verify_command",
    ) {
        return;
    }
    let (_root, runtime) = runtime_with_operator();
    let warnings = file(
        &runtime,
        "Verify access first.\n\n1. Run it\n\n```sh\nmake run\n```\n",
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("no verify command")),
        "an early sentence and a later step fence are not a verify command: {warnings:?}"
    );

    let warnings = file(
        &runtime,
        "Do the thing.\n\n1. Run it\n\n```sh\nmake run\n```\n\nVerify:\n\n```sh\nmake check\n```\n",
    );
    assert!(
        warnings
            .iter()
            .all(|warning| !warning.contains("no verify command")),
        "a Verify heading followed by one fence is the command: {warnings:?}"
    );
}
