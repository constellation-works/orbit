#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env::{self, harden_dir};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

#[test]
fn cli_docs_list_and_show_json() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "docs/pattern.md",
        "---\ntype: pattern\nsummary: RAII guard pattern\ntags: [rust, guard]\nrelated_artifacts: [ORB-00160]\n---\n# Guard\n\nBody\n",
    );
    workspace.write(".orbit/private/hidden.md", "# Hidden state\n");

    let listed = workspace.run_json(&["docs", "list", "--json"], "docs list");
    let rows = listed.as_array().expect("array");
    assert!(rows.iter().any(|row| row["path"] == "docs/pattern.md"));
    assert!(
        rows.iter()
            .all(|row| { !row["path"].as_str().expect("path").starts_with(".orbit/") })
    );

    let shown = workspace.run_json(&["docs", "show", "docs/pattern.md", "--json"], "docs show");
    assert_eq!(shown["frontmatter"]["type"], "pattern");
    assert!(shown["body"].as_str().expect("body").contains("# Guard"));
}

/// The global `--format json` must yield the same document `--json` does;
/// before the commands built one payload, `--format json` fell through to
/// the human `println!` lines and `jq` choked on `Path: ...`.
#[test]
fn cli_docs_show_and_migrate_honor_the_global_json_format() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "docs/pattern.md",
        "---\ntype: pattern\nsummary: RAII guard pattern\ntags: [rust, guard]\n---\n# Guard\n\nBody\n",
    );

    let shown = workspace.run_json(
        &["docs", "show", "docs/pattern.md", "--format", "json"],
        "docs show --format json",
    );
    assert_eq!(shown["frontmatter"]["type"], "pattern");
    assert_eq!(shown["path"], "docs/pattern.md");

    let human = workspace.run(&["docs", "show", "docs/pattern.md"], "docs show");
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("Path: docs/pattern.md"), "{stdout}");
    assert!(stdout.contains("# Guard"), "{stdout}");

    let migrated = workspace.run_json(
        &["docs", "migrate", "--format", "json"],
        "docs migrate --format json",
    );
    assert_eq!(migrated["dry_run"], json!(true));
    assert!(migrated["changed"].is_array(), "{migrated}");
}

#[test]
fn cli_docs_migrate_is_dry_run_by_default_and_confirm_applies() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "docs/design-patterns/legacy.md",
        "# Legacy Pattern\n\nBody\n",
    );
    let path = workspace.work.join("docs/design-patterns/legacy.md");
    let before = fs::read_to_string(&path).expect("read legacy doc");

    let preview = workspace.run_json(&["docs", "migrate", "--json"], "docs migrate preview");
    assert_eq!(preview["dry_run"], true);
    assert_eq!(
        fs::read_to_string(&path).expect("read previewed doc"),
        before
    );

    let applied = workspace.run_json(
        &["docs", "migrate", "--confirm", "--json"],
        "docs migrate apply",
    );
    assert_eq!(applied["dry_run"], false);
    let after = fs::read_to_string(&path).expect("read migrated doc");
    assert_ne!(after, before);
    assert!(after.starts_with("---\n"));
}

#[test]
fn cli_orbit_search_limit_help_describes_total_round_robin_limit() {
    let workspace = TestWorkspace::new();

    let output = workspace.run(&["search", "--help"], "orbit search help");
    let help = String::from_utf8_lossy(&output.stdout);

    assert!(help.contains("Maximum total results returned"));
    assert!(help.contains("round-robin per kind"));
    assert!(help.contains("[default: 10]"));
    assert!(!help.contains("ADRs use lexical matching regardless of --hybrid."));
    assert!(!help.contains("Index coverage note"));
    assert!(!help.contains("learnings and ADRs use lexical matching"));
}

#[test]
fn cli_orbit_search_path_notes_doc_branch_skip_in_json_and_table_modes() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "docs/path-note.md",
        "---\ntype: context\nsummary: path note\n---\nBody\n",
    );

    let response = workspace.run_json(
        &["search", "path", "crates/orbit-cli/", "--json"],
        "orbit search path json",
    );
    let notes = response["notes"].as_array().expect("notes");
    assert!(
        notes.iter().any(|note| {
            let note = note.as_str().expect("note");
            note.contains("doc branch skipped") && note.contains("--path")
        }),
        "JSON notes should mention doc branch and --path: {notes:?}"
    );

    let output = workspace.run(
        &["search", "path", "crates/orbit-cli/"],
        "orbit search path table",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("note: ")
            && stderr.contains("doc branch skipped")
            && stderr.contains("--path"),
        "table-mode stderr should include prefixed note: {stderr}"
    );
}

#[test]
fn cli_orbit_search_hybrid_doc_json_reports_lexical_fallback_note() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "docs/hybrid-note.md",
        "---\ntype: context\nsummary: hybrid-note\n---\nhybrid-note body\n",
    );

    let response = workspace.run_json(
        &[
            "search",
            "hybrid-note",
            "--hybrid",
            "--kind",
            "doc",
            "--json",
        ],
        "orbit search hybrid doc",
    );
    let notes = response["notes"].as_array().expect("notes");
    assert!(
        notes.iter().any(|note| {
            note.as_str()
                .expect("note")
                .contains("falling back to lexical doc search")
        }),
        "hybrid doc notes should preserve lexical fallback warning: {notes:?}"
    );
}

#[test]
fn cli_docs_add_is_idempotent_and_rejects_dot_orbit() {
    let workspace = TestWorkspace::new();
    fs::create_dir_all(workspace.work.join("extra-docs")).expect("extra docs");
    let first = workspace.run_json(&["docs", "add", "extra-docs", "--json"], "docs add");
    assert_eq!(first["added"], true);
    let second = workspace.run_json(&["docs", "add", "extra-docs", "--json"], "docs add again");
    assert_eq!(second["added"], false);

    let output = run_orbit(
        &workspace.work,
        &workspace.home,
        &["docs", "add", ".orbit", "--json"],
    );
    assert!(!output.status.success());
    let payload: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|_| serde_json::from_slice(&output.stdout).expect("json error payload"));
    assert_eq!(payload["code"], "invalid_input");
}

#[test]
fn cli_task_show_with_context_includes_related_docs_json() {
    let workspace = TestWorkspace::new();
    workspace.write("crates/orbit-cli/src/command/docs.rs", "// fixture\n");
    workspace.write(
        "docs/cli.md",
        "---\ntype: design\nsummary: CLI docs command design\npaths: [\"crates/orbit-cli/**\"]\n---\n# CLI Docs\n\nBody\n",
    );

    let task = workspace.run_json(
        &[
            "task",
            "add",
            "--title",
            "Wire docs",
            "--description",
            "Exercise docs context injection.",
            "--context",
            "file:crates/orbit-cli/src/command/docs.rs",
            "--complexity",
            "medium",
            "--json",
        ],
        "task add",
    );
    let task_id = task["id"].as_str().expect("task id");

    let shown = workspace.run_json(
        &[
            "task",
            "show",
            task_id,
            "--with-context",
            "--max-docs",
            "1",
            "--json",
        ],
        "task show with context",
    );
    assert_eq!(
        shown["related_docs"],
        json!([
            {
                "path": "docs/cli.md",
                "type": "design",
                "summary": "CLI docs command design",
                "excerpt": "CLI Docs",
                "matched_by": ["path:crates/orbit-cli/**"]
            }
        ])
    );

    let plain = workspace.run_json(&["task", "show", task_id, "--json"], "task show");
    assert!(plain.get("related_docs").is_none());
}

#[test]
fn cli_task_show_with_context_returns_empty_docs_when_roots_are_empty() {
    let workspace = TestWorkspace::new();
    workspace.write(".orbit/config.toml", "[docs]\nroots = []\n");
    workspace.write("crates/orbit-cli/src/command/docs.rs", "// fixture\n");
    workspace.write(
        "docs/cli.md",
        "---\ntype: design\nsummary: CLI docs command design\npaths: [\"crates/orbit-cli/**\"]\n---\n# CLI Docs\n",
    );
    let task = workspace.run_json(
        &[
            "task",
            "add",
            "--title",
            "No roots",
            "--description",
            "Exercise empty docs roots.",
            "--context",
            "file:crates/orbit-cli/src/command/docs.rs",
            "--complexity",
            "low",
            "--json",
        ],
        "task add",
    );
    let task_id = task["id"].as_str().expect("task id");

    let shown = workspace.run_json(
        &["task", "show", task_id, "--with-context", "--json"],
        "task show with context",
    );

    assert_eq!(shown["related_docs"], json!([]));
}

#[test]
fn docs_tools_are_cli_only_and_visible_in_tool_list_all() {
    let workspace = TestWorkspace::new();
    workspace.write(
        "docs/context.md",
        "---\ntype: context\nsummary: Context document\n---\nBody\n",
    );

    let tools = workspace.run_json(&["tool", "list", "--json"], "tool list");
    let default_names = tools
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect::<Vec<_>>();
    let all_tools = workspace.run_json(&["tool", "list", "--json", "--all"], "tool list --all");
    let all_tools = all_tools.as_array().expect("all tools");
    for name in [
        "orbit.docs.list",
        "orbit.docs.show",
        "orbit.docs.add",
        "orbit.docs.index",
        "orbit.docs.migrate",
    ] {
        assert!(
            !default_names.contains(&name),
            "docs tool must be hidden from default tool list: {name}"
        );
        let tool = all_tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing docs tool from --all: {name}"));
        assert_eq!(tool["status"], "inactive");
    }
    // ORB-00202: `orbit.docs.search` deleted in phase 2.
    assert!(
        !default_names.contains(&"orbit.docs.search"),
        "orbit.docs.search must be deleted in phase 2"
    );

    let output = run_orbit(
        &workspace.work,
        &workspace.home,
        &["tool", "run", "orbit.docs.list", "--input", "{}"],
    );
    assert!(
        !output.status.success(),
        "inactive docs tool unexpectedly succeeded through tool run"
    );
    // Errors, including the JSON payload, are on stderr [ORB-10570].
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("inactive"));

    let output = workspace.run_json(&["docs", "list", "--json"], "docs list");
    assert!(!output.as_array().expect("array").is_empty());
}

#[test]
#[cfg(unix)]
fn cli_docs_index_is_semantic_docs_alias_for_json_output() {
    let workspace = TestWorkspace::new();
    workspace.write_mock_companion();
    workspace.write(
        "docs/context.md",
        "---\ntype: context\nsummary: Context document\ntags: [semantic]\n---\nBody\n",
    );

    let docs =
        workspace.run_json_with_companion(&["docs", "index", "--force", "--json"], "docs index");
    let semantic = workspace.run_json_with_companion(
        &["semantic", "index", "--kind", "docs", "--force", "--json"],
        "semantic index docs",
    );

    assert_eq!(semantic, docs);
}

#[test]
#[cfg(unix)]
fn cli_semantic_index_all_keeps_task_rows_when_docs_fail() {
    let workspace = TestWorkspace::new();
    workspace.write_mock_companion();
    workspace.run_json(
        &[
            "task",
            "add",
            "--title",
            "Partial progress",
            "--description",
            "Exercise all-kind resilience.",
            "--acceptance-criteria",
            "task rows persist",
            "--complexity",
            "low",
            "--json",
        ],
        "task add",
    );
    workspace.write(
        "docs/broken.md",
        "---\ntype: context\nsummary: Broken doc\n---\nBody\n",
    );
    let broken = workspace.work.join("docs/broken.md");
    make_unreadable(&broken);

    let output = workspace.run_failure_with_companion(
        &["semantic", "index", "--kind", "all", "--json"],
        "semantic index all with unreadable docs",
    );
    restore_readable(&broken);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("read"),
        "stderr should explain docs read failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stats =
        workspace.run_json_with_companion(&["semantic", "stats", "--json"], "semantic stats");
    let rows = stats["rows"]["counts"].as_array().expect("counts");
    assert!(
        rows.iter()
            .any(|row| row["source_kind"] == "task" && row["rows"].as_u64().unwrap_or(0) > 0),
        "task rows should be present after docs failure: {rows:?}"
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
    );
    assert!(
        output.status.success(),
        "child workspace init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

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
    let docs_output = run_orbit(&work, &home, &["docs", "list", "--json"]);
    assert!(
        docs_output.status.success(),
        "child docs list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&docs_output.stdout),
        String::from_utf8_lossy(&docs_output.stderr)
    );
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
fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
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
    home: PathBuf,
    work: PathBuf,
    companion: PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        harden_dir(temp.path());
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let companion = temp.path().join("mock-companion");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&work).expect("work");
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
        };
        workspace.run(
            &["workspace", "init", "--name", "docs-cli-test"],
            "workspace init",
        );
        workspace
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.work.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, content).expect("write file");
    }

    fn run(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit(&self.work, &self.home, args);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_json(&self, args: &[&str], label: &str) -> Value {
        let output = self.run(args, label);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{label} produced invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    #[cfg(unix)]
    fn run_with_companion(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit_with_companion(&self.work, &self.home, args, Some(&self.companion));
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[cfg(unix)]
    fn run_json_with_companion(&self, args: &[&str], label: &str) -> Value {
        let output = self.run_with_companion(args, label);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{label} produced invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    #[cfg(unix)]
    fn run_failure_with_companion(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit_with_companion(&self.work, &self.home, args, Some(&self.companion));
        assert!(
            !output.status.success(),
            "{label} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[cfg(unix)]
    fn write_mock_companion(&self) {
        write_executable(
            &self.companion,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  if [ -z "$id" ]; then
    id=0
  fi
  case "$line" in
    *'"method":"info"'*)
      printf '{"id":%s,"result":{"model_id":"bge-small-en-v1.5","dim":2,"max_input_tokens":512,"version":"0.3.1"}}\n' "$id"
      ;;
    *'"method":"token_count"'*)
      printf '{"id":%s,"result":{"tokens":1}}\n' "$id"
      ;;
    *'"method":"embed"'*)
      case "$line" in
        *'"texts":["foo"]'*|*semantic-target*)
          printf '{"id":%s,"result":{"vectors":[[1.0,0.0]]}}\n' "$id"
          ;;
        *)
          printf '{"id":%s,"result":{"vectors":[[0.0,1.0]]}}\n' "$id"
          ;;
      esac
      ;;
    *'"method":"exit"'*)
      printf '{"id":%s,"result":{"ok":true}}\n' "$id"
      exit 0
      ;;
    *)
      printf '{"id":%s,"error":{"code":"unknown","message":"unknown request"}}\n' "$id"
      ;;
  esac
done
"#,
        );
    }
}

fn run_orbit(work: &PathBuf, home: &PathBuf, args: &[&str]) -> Output {
    run_orbit_with_companion(work, home, args, None)
}

fn run_orbit_with_companion(
    work: &PathBuf,
    home: &PathBuf,
    args: &[&str],
    companion: Option<&std::path::Path>,
) -> Output {
    let mut cmd = cargo_bin_cmd!("orbit");
    // ORB-11300: `HOME`/`ORBIT_HOME` do not outrank the inherited
    // `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair, so clear the whole
    // ambient-authority set before pinning this fixture's own paths.
    test_env::clear_inherited_authority(|name| {
        cmd.env_remove(name);
    });
    cmd.current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_HOME", home.join(".orbit-global"))
        .env_remove("ORBIT_SEARCH_COMPANION")
        .env_remove("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE")
        .args(args);
    if let Some(path) = companion {
        cmd.env("ORBIT_SEARCH_COMPANION", path)
            .env("ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE", "1");
    }
    cmd.output().expect("run orbit")
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}

#[cfg(unix)]
fn make_unreadable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(path, permissions).expect("chmod unreadable");
}

#[cfg(unix)]
fn restore_readable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(path, permissions).expect("chmod readable");
}
