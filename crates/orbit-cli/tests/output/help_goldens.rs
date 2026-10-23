//! `help_goldens/**/*.txt` freeze the shipped `--help` contract of the
//! `orbit` binary. Drift is a consumer-visible CLI surface change. If the new
//! help is intentional, regenerate with `ORBIT_UPDATE_HELP_GOLDENS=1 cargo test
//! -p orbit-cli --test output help_goldens::` (or `make goldens UPDATE=1`) and review
//! the diff.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::tempdir;

const UPDATE_HELP_GOLDENS_ENV: &str = "ORBIT_UPDATE_HELP_GOLDENS";

/// Each argv (after `orbit`) whose `--help` is pinned, and its golden path
/// under `tests/help_goldens/`.
const CASES: &[(&[&str], &str)] = &[
    (&[], "root.txt"),
    (&["run"], "run/root.txt"),
    (&["run", "auto"], "run/auto.txt"),
    (&["run", "readiness"], "run/readiness.txt"),
    (&["init"], "init.txt"),
    (&["host", "add"], "host/add.txt"),
    (&["host", "remove"], "host/remove.txt"),
    (&["doctor"], "doctor.txt"),
    (&["config", "show"], "config-show.txt"),
    (&["search"], "search.txt"),
    (&["search", "reindex"], "search-reindex.txt"),
    (&["codebase"], "codebase/root.txt"),
    (&["codebase", "analytics"], "codebase/analytics.txt"),
    (&["codebase", "show"], "codebase/show.txt"),
    (&["codebase", "related"], "codebase/related.txt"),
    (&["agents"], "agents/root.txt"),
    (&["agents", "scorecard"], "agents/scorecard.txt"),
    (&["agents", "show"], "agents/show.txt"),
    (&["orchestrator"], "orchestrator/root.txt"),
    (&["orchestrator", "scorecard"], "orchestrator/scorecard.txt"),
    (&["orchestrator", "show"], "orchestrator/show.txt"),
    (&["gc"], "gc/root.txt"),
    (&["gc", "audit"], "gc/audit.txt"),
    (&["gc", "deployments"], "gc/deployments.txt"),
    (&["gc", "journal"], "gc/journal.txt"),
    (&["gc", "runs"], "gc/runs.txt"),
    (&["gc", "tmp"], "gc/tmp.txt"),
    (&["gc", "worktrees"], "gc/worktrees.txt"),
    (&["run", "history"], "run/history.txt"),
    (&["run", "show"], "run/show.txt"),
    (&["run", "cancel"], "run/cancel.txt"),
    (&["run", "agent"], "run/agent.txt"),
    (&["run", "job"], "run/job.txt"),
    (&["run", "task-pilot"], "run/task-pilot.txt"),
    (&["job", "resume"], "job/resume.txt"),
    (&["run", "logs"], "run/logs.txt"),
    (&["run", "profiles"], "run/profiles.txt"),
    (&["friction"], "friction/root.txt"),
    (&["friction", "add"], "friction/add.txt"),
    (&["friction", "list"], "friction/list.txt"),
    (&["friction", "show"], "friction/show.txt"),
    (&["friction", "stats"], "friction/stats.txt"),
    (&["friction", "tags"], "friction/tags.txt"),
    (&["friction", "update"], "friction/update.txt"),
    (&["friction", "resolve"], "friction/resolve.txt"),
    (&["friction", "rehome"], "friction/rehome.txt"),
    (&["auto-task"], "auto-task/root.txt"),
    (&["auto-task", "add"], "auto-task/add.txt"),
    (&["auto-task", "list"], "auto-task/list.txt"),
    (&["auto-task", "show"], "auto-task/show.txt"),
    (&["auto-task", "update"], "auto-task/update.txt"),
    (&["auto-task", "toggle"], "auto-task/toggle.txt"),
    (&["auto-task", "delete"], "auto-task/delete.txt"),
    (&["auto-task", "restore"], "auto-task/restore.txt"),
    (&["auto-task", "mint"], "auto-task/mint.txt"),
    (&["auto-task", "recover"], "auto-task/recover.txt"),
    (&["auto-task", "reset"], "auto-task/reset.txt"),
    (&["task"], "task/root.txt"),
    (&["task", "add"], "task/add.txt"),
    (&["task", "update"], "task/update.txt"),
    (&["task", "list"], "task/list.txt"),
    (&["task", "eligible"], "task/eligible.txt"),
    (&["task", "show"], "task/show.txt"),
    (&["task", "lint"], "task/lint.txt"),
    (&["task", "artifact"], "task/artifact.txt"),
    (&["task", "artifact", "put"], "task/artifact-put.txt"),
    (&["task", "artifact", "get"], "task/artifact-get.txt"),
    (&["task", "locks"], "task/locks.txt"),
    (&["task", "locks", "list"], "task/locks-list.txt"),
    (
        &["task", "locks", "contention"],
        "task/locks-contention.txt",
    ),
    (&["task", "locks", "reserve"], "task/locks-reserve.txt"),
    (&["task", "locks", "release"], "task/locks-release.txt"),
    (&["task", "review-reset"], "task/review-reset.txt"),
    (&["task", "requeue"], "task/requeue.txt"),
    (&["task", "retry-handoff"], "task/retry-handoff.txt"),
    (&["task", "reconcile-review"], "task/reconcile-review.txt"),
    (
        &["task", "reconcile-review", "inspect"],
        "task/reconcile-review-inspect.txt",
    ),
    (
        &["task", "reconcile-review", "submit"],
        "task/reconcile-review-submit.txt",
    ),
    (
        &["task", "reconcile-review", "status"],
        "task/reconcile-review-status.txt",
    ),
    (
        &["task", "reconcile-review", "accept-baseline"],
        "task/reconcile-review-accept-baseline.txt",
    ),
    (&["plugin"], "plugin/root.txt"),
    (&["plugin", "upgrade"], "plugin/upgrade.txt"),
    (&["plugin", "remove"], "plugin/remove.txt"),
    (&["plugin", "validate"], "plugin/validate.txt"),
    (&["plugin", "scaffold"], "plugin/scaffold.txt"),
    (&["plugin", "test"], "plugin/test.txt"),
    (&["mcp", "init"], "mcp/init.txt"),
    (&["mcp", "listen"], "mcp/listen.txt"),
    (&["tool", "run"], "tool/run.txt"),
    (&["web", "serve"], "web/serve.txt"),
    (&["web", "connect"], "web/connect.txt"),
];

fn golden_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/help_goldens")
        .join(relative)
}

#[test]
fn help_matches_the_shipped_surface() {
    // An empty HOME and cwd: no workspace and no installed plugin groups, so
    // the help is the binary's own surface.
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");
    let update = std::env::var(UPDATE_HELP_GOLDENS_ENV).as_deref() == Ok("1");

    for (args, relative) in CASES {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ORBIT_FORMAT")
            .env_remove("ORBIT_SKIP_HOST_PREREQUISITES")
            .args(*args)
            .arg("--help")
            .output()
            .expect("run orbit --help");
        assert!(
            output.status.success(),
            "`orbit {} --help` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = String::from_utf8(output.stdout)
            .expect("help is UTF-8")
            .replace(env!("CARGO_PKG_VERSION"), "<VERSION>");
        let path = golden_path(relative);
        if update {
            fs::write(&path, &actual)
                .unwrap_or_else(|err| panic!("write help golden {}: {err}", path.display()));
            continue;
        }
        let expected = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read help golden {}: {err}", path.display()));
        assert_eq!(
            actual,
            expected,
            "`orbit {} --help` drifted from {relative}. If the new help is intentional, \
             regenerate with `{UPDATE_HELP_GOLDENS_ENV}=1 cargo test -p orbit-cli --test \
             help_goldens` or `make goldens UPDATE=1`, then review the diff.",
            args.join(" ")
        );
    }
}
