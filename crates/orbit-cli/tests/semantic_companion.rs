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

/// ORB-12086 regression: a hostile ancestor directory carrying both a `.git`
/// marker and a conflicting `.orbit/config.yaml` identity must not capture a
/// child `workspace init`. Root discovery's legacy git-repo-root fallback
/// walks ancestors for `.git`, so without the child's own boundary marker it
/// would otherwise resolve the ancestor's `.orbit` as the workspace root —
/// exactly how the original bug produced a shared `/tmp/.orbit` that later
/// fixtures then refused as an identity conflict.
#[test]
fn workspace_init_ignores_hostile_ancestor_git_and_orbit_directories() {
    let parent = tempdir().expect("hostile parent tempdir");
    harden_dir(parent.path());
    fs::create_dir_all(parent.path().join(".git")).expect("seed hostile parent git marker");
    let parent_orbit = parent.path().join(".orbit");
    fs::create_dir_all(&parent_orbit).expect("seed hostile parent .orbit");
    fs::write(
        parent_orbit.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_hostile-parent\n",
    )
    .expect("seed hostile parent config");
    let home = parent.path().join("nested/home");
    let work = parent.path().join("nested/work");
    fs::create_dir_all(&home).expect("create nested home");
    fs::create_dir_all(&work).expect("create nested work");
    fs::create_dir_all(work.join(".git")).expect("seed child git boundary");

    let parent_orbit_before = snapshot_tree(&parent_orbit);
    let parent_top_level_before = top_level_entries(parent.path());

    let output = run_orbit(
        &work,
        &home,
        &["workspace", "init", "--name", "hostile-child"],
        None,
    );
    assert_success("hostile-parent workspace init", &output);

    let child_config_path = work.join(".orbit").join("config.yaml");
    assert!(
        child_config_path.exists(),
        "child workspace must bind its own .orbit under work, not the hostile ancestor"
    );
    let child_config = fs::read_to_string(&child_config_path).expect("read child config");
    assert!(
        child_config.contains("ws_hostile-child"),
        "child config should reflect the child's own workspace identity: {child_config}"
    );

    // Exercise roots=[] against the child's own config in this hostile-parent
    // scenario: the empty override must be honored from the child's own
    // .orbit/config.toml, never inherited or shadowed by ancestor state.
    fs::write(
        work.join(".orbit").join("config.toml"),
        "[docs]\nroots = []\n",
    )
    .expect("seed child docs roots override");
    fs::create_dir_all(work.join("docs")).expect("create child docs dir");
    fs::write(
        work.join("docs/cli.md"),
        "---\ntype: design\nsummary: child doc\n---\n# Child Doc\n\nBody\n",
    )
    .expect("seed child doc");
    let docs_output = run_orbit(&work, &home, &["docs", "list", "--json"], None);
    assert_success("child docs list", &docs_output);
    let docs: Value = serde_json::from_slice(&docs_output.stdout).expect("docs list JSON");
    assert_eq!(
        docs,
        json!([]),
        "roots=[] must return no docs even with a hostile ancestor present: {docs}"
    );

    let parent_orbit_after = snapshot_tree(&parent_orbit);
    assert_eq!(
        parent_orbit_after, parent_orbit_before,
        "hostile ancestor .orbit tree must remain byte-for-byte unchanged"
    );
    let parent_top_level_after = top_level_entries(parent.path());
    assert_eq!(
        parent_top_level_after, parent_top_level_before,
        "hostile ancestor directory must gain no new top-level entries"
    );
}

/// Recursively snapshots every file under `root` as `(relative path,
/// contents)`, sorted for deterministic comparison.
fn snapshot_tree(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("relative path")
                    .to_path_buf();
                let contents = fs::read(&path).expect("read file");
                entries.push((relative, contents));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Sorted top-level entry names directly under `root`.
fn top_level_entries(root: &Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(root)
        .expect("read_dir")
        .map(|entry| entry.expect("dir entry").file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
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
        // ORB-12086: seed a child `.git` marker so root discovery's
        // walk-up boundary stops at `work` itself. Without it, an ambient
        // `.git`/`.orbit` above the OS temp root can capture this fixture's
        // `workspace init` into that ancestor instead of `work/.orbit`.
        fs::create_dir_all(work.join(".git")).expect("seed child git boundary");

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
