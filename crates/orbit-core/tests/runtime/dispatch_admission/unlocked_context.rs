//! Declared context that takes no context lock
//! (`workflow.unlocked_context_paths`, default `["**/*.md"]`).
//!
//! Two tasks that share only a Markdown file beside their own code are
//! admitted together; the agent still receives the file as context. A task
//! that declares only matching paths still locks them, the build and CI
//! definitions keep locking, and `[]` restores the lock on every entry.

use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    PressureProbe, Seed, admitted, list_backlog_tasks, readiness_task, reserve_locks, running_run,
    runtime, seed, write_files,
};

const GUIDE: &str = "file:docs/guide.md";

fn task(runtime: &OrbitRuntime, title: &str, status: TaskStatus, files: &[&str]) -> String {
    seed(
        runtime,
        Seed {
            title,
            status,
            context_files: Some(files),
            ..Seed::default()
        },
    )
    .id
}

fn wave(runtime: &OrbitRuntime) -> Value {
    let drain = running_run(runtime, "workspace_auto_pipeline", json!({}));
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify")
}

fn conflicts_of<'a>(output: &'a Value, task: &str) -> &'a Value {
    &output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
        .unwrap_or_else(|| panic!("{task} is not excluded: {output:#}"))["conflicts"]
}

/// Under the default, two backlog tasks sharing only a Markdown file beside
/// distinct code share one wave and are both ready; an active holder of that
/// file excludes neither, a reservation holds only the code, and the agent
/// envelope still lists the Markdown file.
fn shared_markdown_beside_distinct_code_admits_both_and_stays_in_context() {
    let (_root, runtime, repo) = runtime();
    write_files(
        &repo,
        &["docs/guide.md", "src/a.rs", "src/b.rs", "src/held.rs"],
    );
    let first = task(
        &runtime,
        "first",
        TaskStatus::Backlog,
        &[GUIDE, "file:src/a.rs"],
    );
    let second = task(
        &runtime,
        "second",
        TaskStatus::Backlog,
        &[GUIDE, "file:src/b.rs"],
    );

    let wave = wave(&runtime);
    assert_eq!(
        wave["loose_task_ids"],
        json!([first, second]),
        "a shared Markdown file must not serialize the wave: {wave:#}"
    );
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    for id in [&first, &second] {
        let ready = readiness_task(&readiness, id);
        assert_eq!(ready["eligible"], true, "{ready}");
        assert_eq!(ready["reason"], "ready", "{ready}");
    }

    task(
        &runtime,
        "holder",
        TaskStatus::InProgress,
        &[GUIDE, "file:src/held.rs"],
    );
    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(admitted(&output), vec![first.clone(), second.clone()]);
    assert_eq!(output["excluded"], json!([]), "{output:#}");

    let grant = reserve_locks(&runtime, &first);
    assert_eq!(grant["reserved"], true, "{grant}");
    assert_eq!(grant["reserved_files"], json!(["file:src/a.rs"]), "{grant}");
    let grant = reserve_locks(&runtime, &second);
    assert_eq!(
        grant["reserved"], true,
        "the first reservation holds no Markdown: {grant}"
    );

    let envelope = RuntimeHost::task_context_for_agent_input(
        &runtime,
        "jrun-envelope",
        "implement",
        &json!({"task_id": first}),
    )
    .unwrap()
    .expect("a task envelope");
    assert_eq!(
        envelope["context_files"],
        json!([GUIDE, "file:src/a.rs"]),
        "an unlocked entry stays in the agent's context: {envelope:#}"
    );
}

/// A task that declares only Markdown still locks it against another such
/// task. A Makefile, CI workflow or code entry keeps locking beside Markdown,
/// and a conflict names only that entry.
fn docs_only_tasks_and_build_ci_and_code_paths_keep_their_locks() {
    let (_root, runtime, repo) = runtime();
    write_files(
        &repo,
        &[
            "docs/guide.md",
            "docs/other.md",
            "Makefile",
            ".github/workflows/ci.yml",
            "src/a.rs",
            "src/b.rs",
        ],
    );
    let rewrite = task(&runtime, "rewrite", TaskStatus::InProgress, &[GUIDE]);
    let second_rewrite = task(&runtime, "second rewrite", TaskStatus::Backlog, &[GUIDE]);
    let gates = task(
        &runtime,
        "gates",
        TaskStatus::InProgress,
        &[
            "file:docs/other.md",
            "file:Makefile",
            "file:.github/workflows/ci.yml",
            "file:src/a.rs",
        ],
    );
    let mut locked = Vec::new();
    for file in [
        "file:Makefile",
        "file:.github/workflows/ci.yml",
        "file:src/a.rs",
    ] {
        let id = task(
            &runtime,
            file,
            TaskStatus::Backlog,
            &["file:docs/other.md", file],
        );
        locked.push((id, file));
    }
    let beside_rewrite = task(
        &runtime,
        "beside rewrite",
        TaskStatus::Backlog,
        &[GUIDE, "file:src/b.rs"],
    );

    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(admitted(&output), vec![beside_rewrite], "{output:#}");
    assert_eq!(
        conflicts_of(&output, &second_rewrite),
        &json!([{"requested_file": GUIDE, "locking_task_id": rewrite}]),
        "two docs-only rewrites of one file must not run together"
    );
    for (id, file) in &locked {
        assert_eq!(
            conflicts_of(&output, id),
            &json!([{"requested_file": file, "locking_task_id": gates}]),
            "{file} keeps its lock; the shared Markdown takes none"
        );
    }
    let grant = reserve_locks(&runtime, &second_rewrite);
    assert_eq!(grant["reserved"], false, "{grant}");
}

/// `workflow.unlocked_context_paths = []` locks every declared entry again,
/// so a shared Markdown file serializes the mixed tasks it used to admit.
fn an_empty_unlocked_list_restores_the_markdown_lock() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\nunlocked_context_paths = []\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(PressureProbe::calm());
    write_files(
        &repo,
        &["docs/guide.md", "src/a.rs", "src/b.rs", "src/held.rs"],
    );
    let first = task(
        &runtime,
        "first",
        TaskStatus::Backlog,
        &[GUIDE, "file:src/a.rs"],
    );
    let second = task(
        &runtime,
        "second",
        TaskStatus::Backlog,
        &[GUIDE, "file:src/b.rs"],
    );

    let wave = wave(&runtime);
    assert_eq!(wave["loose_task_ids"], json!([first]), "{wave:#}");

    let holder = task(
        &runtime,
        "holder",
        TaskStatus::InProgress,
        &[GUIDE, "file:src/held.rs"],
    );
    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(admitted(&output), Vec::<String>::new(), "{output:#}");
    for id in [&first, &second] {
        assert_eq!(
            conflicts_of(&output, id),
            &json!([{"requested_file": GUIDE, "locking_task_id": holder}])
        );
    }
}

crate::scenario_table!(
    unlocked_context_scenarios,
    shared_markdown_beside_distinct_code_admits_both_and_stays_in_context,
    docs_only_tasks_and_build_ci_and_code_paths_keep_their_locks,
    an_empty_unlocked_list_restores_the_markdown_lock
);
