#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env::{self, harden_dir};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

#[test]
#[cfg(unix)]
fn cli_task_mutation_does_not_launch_background_companion() {
    let workspace = TestWorkspace::new();
    workspace.write_instrumented_companion();
    let task = workspace.add_task_without_companion();
    let task_id = task["id"].as_str().expect("task id");

    let input = json!({
        "id": task_id,
        "comment": "cli mutation must not spawn a detached companion",
        "model": "gpt-5"
    })
    .to_string();
    let output = workspace.run_with_companion(
        &[
            "tool",
            "run",
            "orbit.task.update",
            "--input",
            &input,
            "--full",
        ],
        "tool run task update",
    );
    assert_stderr_lacks_broken_pipe(&output);
    assert!(
        !workspace.companion_invoked(),
        "CLI task mutation launched a background companion; launch log should stay empty"
    );
}

#[test]
#[cfg(unix)]
fn explicit_semantic_index_launches_companion() {
    let workspace = TestWorkspace::new();
    workspace.write_instrumented_companion();
    workspace.add_task_without_companion();

    workspace.run_with_companion(&["semantic", "index", "--json"], "semantic index");
    assert!(
        workspace.companion_invoked(),
        "explicit `orbit semantic index` must launch the companion; launch log was empty"
    );
}

/// Foreground search runs the companion with inherited stderr, so a broken
/// companion's diagnostics reach the operator. Contrast with
/// [`cli_task_mutation_does_not_launch_background_companion`],
/// where short-lived CLI mutations do not start a background companion.
///
/// Hybrid search degrades to lexical instead of failing when infrastructure
/// is unavailable (ORB-10304). The degradation must stay visible: companion
/// stderr plus an explicit fallback note (ORB-10350).
#[test]
#[cfg(unix)]
fn direct_search_semantic_command_surfaces_companion_stderr() {
    let workspace = TestWorkspace::new();
    workspace.write_failing_companion();

    let output = run_orbit(
        &workspace.work,
        &workspace.home,
        &["search", "anything", "--hybrid", "--kind", "task"],
        Some(&workspace.companion),
    );

    assert_success("hybrid search with a failing companion", &output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("direct semantic failure detail"),
        "direct search command should inherit companion stderr\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("falling back to lexical task search"),
        "lexical degradation must be reported, not silent\nstderr:\n{stderr}"
    );
}

#[test]
#[cfg(unix)]
fn tool_run_hybrid_search_without_companion_returns_lexical_results() {
    let workspace = TestWorkspace::new();
    let task = workspace.add_task_without_companion();
    let task_id = task["id"].as_str().expect("task id");
    let embed_root = workspace.home.join(".orbit").join("embed");
    assert!(
        !embed_root.exists(),
        "test must start without companion state"
    );

    let input = json!({
        "query": "Noisy companion regression",
        "hybrid": true,
        "kind": "task",
        "limit": 1,
        "model": "codex"
    })
    .to_string();
    let output = workspace.run_without_companion(
        &["tool", "run", "orbit.search", "--input", &input, "--full"],
        "tool run hybrid search without companion",
    );
    let response: Value = serde_json::from_slice(&output.stdout).expect("search JSON");

    assert_eq!(response["mode"], "lexical");
    assert_eq!(response["results"][0]["id"], task_id);
    assert_eq!(response["results"][0]["source"], "lexical");
    let notes = response["notes"].as_array().expect("fallback notes");
    assert!(notes.iter().any(|note| {
        note.as_str()
            .is_some_and(|note| note.contains("falling back to lexical task search"))
    }));
    assert!(notes.iter().all(|note| {
        note.as_str()
            .is_some_and(|note| !note.contains("orbit semantic install"))
    }));
    assert!(
        !embed_root.exists(),
        "fallback must not install companion state"
    );
}

struct TestWorkspace {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
    companion: std::path::PathBuf,
    invocations: std::path::PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        harden_dir(temp.path());
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let companion = temp.path().join("mock-companion");
        let invocations = temp.path().join("companion-invocations");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(&work).expect("create work");

        let workspace = Self {
            _temp: temp,
            home,
            work,
            companion,
            invocations,
        };
        workspace.run_without_companion(
            &["workspace", "init", "--name", "semantic-companion-test"],
            "initialize workspace",
        );
        workspace
    }

    fn add_task_without_companion(&self) -> Value {
        let output = self.run_without_companion(
            &[
                "task",
                "add",
                "--title",
                "Noisy companion regression",
                "--description",
                "Task used by the semantic indexing stderr regression test.",
                "--acceptance-criteria",
                "task mutation succeeds",
                "--complexity",
                "low",
                "--json",
            ],
            "add task",
        );
        serde_json::from_slice(&output.stdout).expect("task add JSON")
    }

    fn run_without_companion(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit(&self.work, &self.home, args, None);
        assert_success(label, &output);
        output
    }

    fn run_with_companion(&self, args: &[&str], label: &str) -> Output {
        let _ = fs::remove_file(&self.invocations);
        let output = run_orbit(&self.work, &self.home, args, Some(&self.companion));
        assert_success(label, &output);
        output
    }

    #[cfg(unix)]
    fn write_instrumented_companion(&self) {
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' 'execution failed: Broken pipe (os error 32)' >&2
printf '%s\n' invoked >> "{}"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  if [ -z "$id" ]; then
    id=0
  fi
  case "$line" in
    *'"method":"info"'*)
      printf '{{"id":%s,"result":{{"model_id":"bge-small-en-v1.5","dim":2,"max_input_tokens":512,"version":"0.3.1"}}}}\n' "$id"
      ;;
    *'"method":"token_count"'*)
      printf '{{"id":%s,"result":{{"tokens":1}}}}\n' "$id"
      ;;
    *'"method":"embed"'*)
      printf '{{"id":%s,"result":{{"vectors":[[1.0,0.0]]}}}}\n' "$id"
      ;;
    *'"method":"exit"'*)
      printf '{{"id":%s,"result":{{"ok":true}}}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{{"id":%s,"error":{{"code":"unknown","message":"unknown request"}}}}\n' "$id"
      ;;
  esac
done
"#,
            self.invocations.display()
        );
        write_executable(&self.companion, &script);
    }

    #[cfg(unix)]
    fn write_failing_companion(&self) {
        let script = r#"#!/bin/sh
printf '%s\n' 'direct semantic failure detail' >&2
exit 7
"#;
        write_executable(&self.companion, script);
    }

    fn companion_invoked(&self) -> bool {
        fs::read_to_string(&self.invocations)
            .map(|content| !content.trim().is_empty())
            .unwrap_or(false)
    }
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str], companion: Option<&Path>) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: drop inherited registry/workspace authority before the
    // companion-specific overrides below.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_SEARCH_COMPANION")
        .env_remove("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE")
        .args(args);
    if let Some(path) = companion {
        command
            .env("ORBIT_SEARCH_COMPANION", path)
            .env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE", "1");
    }
    command.output().expect("run orbit")
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_stderr_lacks_broken_pipe(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("execution failed: Broken pipe (os error 32)"),
        "background companion stderr leaked into command output\nstderr:\n{stderr}"
    );
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}
