use std::path::{Path, PathBuf};

use clap::Parser;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;
use tempfile::tempdir;

use crate::command::Execute;
use crate::command::{Cli, Commands};

use super::super::artifact::{
    TaskArtifactCommand, TaskArtifactGetArgs, TaskArtifactPutArgs, TaskArtifactSubcommand,
};
use crate::command::task::TaskSubcommand;

#[test]
fn cli_parses_task_artifact_put() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "artifact",
        "put",
        "T1",
        "./summary.md",
        "--path",
        "reports/summary.md",
        "--json",
    ]);

    let Commands::Task(task_command) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Artifact(TaskArtifactCommand {
        command: TaskArtifactSubcommand::Put(args),
    }) = task_command.command
    else {
        panic!("expected task artifact put command");
    };

    assert_eq!(args.id, "T1");
    assert_eq!(args.source_path, PathBuf::from("./summary.md"));
    assert_eq!(args.artifact_path.as_deref(), Some("reports/summary.md"));
    assert!(args.json);
}

#[test]
fn artifact_put_writes_to_task_artifact_store() {
    let (_root, runtime, repo_root) = test_runtime();
    let source = repo_root.join("summary.md");
    std::fs::write(&source, "stored\n").expect("write source");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Artifact store".to_string(),
            description: "Store a task artifact".to_string(),
            workspace_path: Some(repo_root.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .expect("create task");

    TaskArtifactPutArgs {
        id: task.id.clone(),
        source_path: source,
        artifact_path: Some("reports/summary.md".to_string()),
        model: Some("gpt-5".to_string()),
        json: false,
    }
    .execute(&runtime)
    .expect("put artifact");

    let artifacts = runtime
        .get_task_artifacts(&task.id)
        .expect("read task artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "reports/summary.md");
    assert_eq!(artifacts[0].text_content(), Some("stored\n"));
}

#[test]
fn cli_parses_task_artifact_get() {
    let cli = Cli::parse_from([
        "orbit",
        "task",
        "artifact",
        "get",
        "T1",
        "diagrams/flow.png",
        "--out",
        "/tmp/flow.png",
    ]);

    let Commands::Task(task_command) = cli.command else {
        panic!("expected task command");
    };
    let TaskSubcommand::Artifact(TaskArtifactCommand {
        command: TaskArtifactSubcommand::Get(args),
    }) = task_command.command
    else {
        panic!("expected task artifact get command");
    };

    assert_eq!(args.id, "T1");
    assert_eq!(args.path, "diagrams/flow.png");
    assert_eq!(args.out.as_deref(), Some(Path::new("/tmp/flow.png")));
    assert!(!args.json);
}

/// The `attach -> list -> view` round trip an agent is told to use instead of
/// copying files between hosts: bytes written back must be byte-identical.
#[test]
fn artifact_get_writes_binary_bytes_back_out_byte_for_byte() {
    let (_root, runtime, repo_root) = test_runtime();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0x00, 0xFF, 0x10, 0x42, 0x00]);
    let source = repo_root.join("flow.png");
    std::fs::write(&source, &png).expect("write source");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Image artifact".to_string(),
            description: "Store and read back an image".to_string(),
            workspace_path: Some(repo_root.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .expect("create task");

    TaskArtifactPutArgs {
        id: task.id.clone(),
        source_path: source,
        artifact_path: Some("diagrams/flow.png".to_string()),
        model: Some("codex".to_string()),
        json: false,
    }
    .execute(&runtime)
    .expect("put artifact");

    let out = repo_root.join("roundtrip.png");
    TaskArtifactGetArgs {
        id: task.id.clone(),
        path: "diagrams/flow.png".to_string(),
        out: Some(out.clone()),
        json: false,
    }
    .execute(&runtime)
    .expect("get artifact");

    assert_eq!(std::fs::read(&out).expect("read roundtrip"), png);
}

/// Without `--out` a binary artifact must say what to do rather than spray
/// bytes at a terminal.
#[test]
fn artifact_get_refuses_to_print_binary_content_without_an_output_file() {
    let (_root, runtime, repo_root) = test_runtime();
    let source = repo_root.join("flow.png");
    std::fs::write(&source, b"\x89PNG\r\n\x1a\nbody").expect("write source");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Image artifact".to_string(),
            description: "Refuse to print binary".to_string(),
            workspace_path: Some(repo_root.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .expect("create task");
    TaskArtifactPutArgs {
        id: task.id.clone(),
        source_path: source,
        artifact_path: Some("diagrams/flow.png".to_string()),
        model: Some("codex".to_string()),
        json: false,
    }
    .execute(&runtime)
    .expect("put artifact");

    let error = TaskArtifactGetArgs {
        id: task.id.clone(),
        path: "diagrams/flow.png".to_string(),
        out: None,
        json: false,
    }
    .execute(&runtime)
    .expect_err("binary content is not printable");
    assert!(
        error.to_string().contains("--out"),
        "the error should name the remedy: {error}"
    );
}

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}
