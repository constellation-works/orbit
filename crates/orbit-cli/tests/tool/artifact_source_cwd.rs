//! A relative artifact `source_path` resolves against the caller's current
//! directory, observed through the built `orbit tool run` entry point.

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};

use crate::git_repo;

fn orbit_in(dir: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

#[test]
fn checkout_relative_source_from_scratch_folder_names_the_absolute_file() {
    let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    orbit_in(&work, &home)
        .args(["workspace", "init", "--name", "artifact-source-cwd"])
        .assert()
        .success();

    let seed_input = json!({
        "title": "Artifact source directory",
        "description": "Target for a scratch-folder attachment.",
        "acceptance_criteria": ["Attach the scratch file."],
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let seed = orbit_in(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &seed_input,
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let task_id = serde_json::from_slice::<Value>(&seed).expect("seed task JSON")["id"]
        .as_str()
        .expect("seed task ID")
        .to_string();

    // An agent that changed into the scratch folder repeats the
    // checkout-relative prefix, which doubles the path under that directory.
    let scratch = work.join(".orbit/tmp");
    let file = work.join(".orbit/tmp/shots/x.png");
    std::fs::create_dir_all(file.parent().expect("screenshot directory"))
        .expect("screenshot directory");
    std::fs::write(&file, "png-bytes").expect("screenshot");
    let put = |source: &str| {
        let input = json!({
            "id": task_id,
            "path": "shots/x.png",
            "source_path": source,
            "model": "codex",
        })
        .to_string();
        orbit_in(&scratch, &home)
            .args([
                "tool",
                "run",
                "orbit.task.artifact.put",
                "--input",
                &input,
                "--format",
                "json",
            ])
            .assert()
    };

    let refused = put(".orbit/tmp/shots/x.png").failure().get_output().clone();
    assert!(refused.stdout.is_empty(), "{refused:?}");
    let error: Value = serde_json::from_slice(&refused.stderr).expect("JSON tool error");
    let message = error["error"].as_str().expect("error message");
    let absolute = file.canonicalize().expect("canonical screenshot");
    assert!(
        message.contains(&absolute.display().to_string()),
        "the refusal must name the absolute source to pass: {error}"
    );
    assert!(
        !message.contains("No such file"),
        "the refusal must not be a bare missing-file read: {error}"
    );

    // The named absolute path is the exact file, and attaching it succeeds.
    put(&absolute.display().to_string()).success();
    let stored = orbit_in(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.artifact.get",
            "--input",
            &json!({"id": task_id, "path": "shots/x.png"}).to_string(),
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stored: Value = serde_json::from_slice(&stored).expect("artifact get JSON");
    assert_eq!(stored["media_type"], "image/png", "{stored}");
    assert_eq!(stored["size"], "png-bytes".len(), "{stored}");

    // A source missing at both locations is still refused.
    put(".orbit/tmp/shots/missing.png").failure();
}
