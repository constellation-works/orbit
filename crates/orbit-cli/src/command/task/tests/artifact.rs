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
use super::super::artifacts::ArtifactsCommand;
use super::super::show::TaskShowArgs;
use crate::command::CommandOutput;
use crate::command::task::TaskSubcommand;
use crate::output::payload::{Block, View};

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

#[test]
fn task_show_and_artifacts_command_are_metadata_only_and_lazy() {
    let (_root, runtime, repo_root) = test_runtime();

    let text_payload = "A".repeat(100 * 1024);
    let text_source = repo_root.join("large.txt");
    std::fs::write(&text_source, &text_payload).expect("write text source");

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0x00, 0xFF, 0x10, 0x42, 0x00]);
    let png_source = repo_root.join("diagram.png");
    std::fs::write(&png_source, &png).expect("write png source");

    let task = runtime
        .add_task(TaskAddParams {
            title: "Metadata only test".to_string(),
            description: "Artifact listing must be metadata only".to_string(),
            ..Default::default()
        })
        .expect("create task");

    TaskArtifactPutArgs {
        id: task.id.clone(),
        source_path: text_source,
        artifact_path: Some("docs/large.txt".to_string()),
        model: Some("codex".to_string()),
        json: false,
    }
    .execute(&runtime)
    .expect("put large text");

    TaskArtifactPutArgs {
        id: task.id.clone(),
        source_path: png_source,
        artifact_path: Some("diagrams/diagram.png".to_string()),
        model: Some("codex".to_string()),
        json: false,
    }
    .execute(&runtime)
    .expect("put binary png");

    // 1. TaskShowArgs with --fields artifacts returns metadata only
    let CommandOutput::Payload(field_payload) = (TaskShowArgs {
        id: task.id.clone(),
        json: true,
        fields: vec!["artifacts".to_string()],
        with_context: false,
        max_docs: None,
    })
    .execute(&runtime)
    .expect("task show --fields artifacts") else {
        panic!("expected payload");
    };

    let (field_doc, field_view) = field_payload.into_view();
    let artifacts = field_doc.as_array().expect("artifacts field is array");
    assert_eq!(artifacts.len(), 2);
    for item in artifacts {
        assert!(item.get("path").is_some());
        assert!(item.get("media_type").is_some());
        assert!(item.get("size").is_some());
        assert!(item.get("created_by").is_some());
        assert!(item.get("content").is_none(), "content must be omitted");
        assert!(
            item.get("content_base64").is_none(),
            "content_base64 must be omitted"
        );
    }
    let serialized_field_doc = serde_json::to_string(&field_doc).unwrap();
    assert!(
        serialized_field_doc.len() < 1000,
        "serialized metadata document should be small, got {} bytes",
        serialized_field_doc.len()
    );

    let View::Blocks(field_blocks) = field_view else {
        panic!("expected blocks view");
    };
    for block in field_blocks {
        if let Block::Text(text) = block {
            assert!(!text.contains(&text_payload));
            assert!(text.contains("docs/large.txt (text/plain, 102400 bytes)"));
        }
    }

    // 2. Full TaskShowArgs returns metadata only in artifacts field
    let CommandOutput::Payload(full_payload) = (TaskShowArgs {
        id: task.id.clone(),
        json: true,
        fields: vec![],
        with_context: false,
        max_docs: None,
    })
    .execute(&runtime)
    .expect("full task show") else {
        panic!("expected payload");
    };
    let (full_doc, full_view) = full_payload.into_view();
    let full_artifacts = full_doc["artifacts"]
        .as_array()
        .expect("artifacts array in full doc");
    assert_eq!(full_artifacts.len(), 2);
    for item in full_artifacts {
        assert!(item.get("content").is_none());
        assert!(item.get("content_base64").is_none());
    }
    let View::Blocks(full_blocks) = full_view else {
        panic!("expected blocks view");
    };
    for block in full_blocks {
        if let Block::Text(text) = block {
            assert!(!text.contains(&text_payload));
            assert!(text.contains("docs/large.txt (text/plain, 102400 bytes)"));
        }
    }

    // 3. ArtifactsCommand (task: true) returns metadata only
    let CommandOutput::Payload(cmd_payload) = (ArtifactsCommand {
        id: task.id.clone(),
        task: true,
        json: true,
    })
    .execute(&runtime)
    .expect("task artifacts command") else {
        panic!("expected payload");
    };
    let (cmd_doc, cmd_view) = cmd_payload.into_view();
    let cmd_artifacts = cmd_doc.as_array().expect("artifacts array in cmd doc");
    assert_eq!(cmd_artifacts.len(), 2);
    for item in cmd_artifacts {
        assert!(item.get("content").is_none());
        assert!(item.get("content_base64").is_none());
    }
    let View::Blocks(cmd_blocks) = cmd_view else {
        panic!("expected blocks view");
    };
    for block in cmd_blocks {
        if let Block::Text(text) = block {
            assert!(!text.contains(&text_payload));
            assert!(text.contains("--- docs/large.txt (text/plain, 102400 bytes) ---"));
        }
    }

    // 4. Verify artifact retrieval via artifact get preserves content and rejects unknown paths
    let out = repo_root.join("retrieved_large.txt");
    TaskArtifactGetArgs {
        id: task.id.clone(),
        path: "docs/large.txt".to_string(),
        out: Some(out.clone()),
        json: false,
    }
    .execute(&runtime)
    .expect("get large text");
    assert_eq!(
        std::fs::read_to_string(&out).expect("read retrieved"),
        text_payload
    );

    let png_out = repo_root.join("retrieved_diagram.png");
    TaskArtifactGetArgs {
        id: task.id.clone(),
        path: "diagrams/diagram.png".to_string(),
        out: Some(png_out.clone()),
        json: false,
    }
    .execute(&runtime)
    .expect("get binary png");
    assert_eq!(std::fs::read(&png_out).expect("read retrieved png"), png);

    // Unknown artifact path rejected
    let err = TaskArtifactGetArgs {
        id: task.id.clone(),
        path: "docs/nonexistent.txt".to_string(),
        out: Some(repo_root.join("none.txt")),
        json: false,
    }
    .execute(&runtime)
    .expect_err("unknown artifact path should fail");
    assert!(matches!(err, orbit_core::OrbitError::NotFound { .. }));
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
