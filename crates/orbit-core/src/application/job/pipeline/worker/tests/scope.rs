//! Worker scope launch arguments and cgroup limit accounting [ORB-12903].
//!
//! Everything here is pure: the argv handed to `systemd-run`, the unit name,
//! and parsing of `/proc/<pid>/cgroup` and cgroup `*.events` files from a
//! temporary directory. The live scope is exercised by the ignored
//! `contained_fork_bomb_*` supervisor test.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use orbit_config::WorkerContainmentSettings;
use tempfile::TempDir;

use crate::application::job::pipeline::worker::scope::{
    WorkerLimits, WorkerScopeCgroup, event_count, scope_unit_name, scoped_worker_command,
};

fn limits() -> WorkerLimits {
    WorkerLimits::from_settings(&WorkerContainmentSettings {
        enabled: true,
        memory_high: "6G".to_string(),
        memory_max: "8G".to_string(),
        tasks_max: 512,
    })
    .expect("enabled containment yields limits")
}

fn args(command: &Command) -> Vec<&str> {
    command
        .get_args()
        .map(|arg| arg.to_str().expect("utf-8 argument"))
        .collect()
}

#[test]
fn disabled_containment_has_no_limits() {
    let settings = WorkerContainmentSettings {
        enabled: false,
        memory_high: "40%".to_string(),
        memory_max: "50%".to_string(),
        tasks_max: 4096,
    };
    assert_eq!(WorkerLimits::from_settings(&settings), None);
}

/// The worker argv must follow `--` unchanged, and the base command's cwd and
/// environment edits — notably the `ORBIT_ROOT` removal that pins workspace
/// identity [ORB-11998] — must survive the wrap, because `systemd-run --scope`
/// execs the worker with its own environment and directory.
#[test]
fn scoped_command_runs_the_worker_argv_under_the_configured_limits() {
    let mut base = Command::new("/opt/orbit/bin/orbit");
    base.args(["job", "run-pipeline-worker", "jrun-1"])
        .current_dir("/work/repo")
        .env("LLVM_PROFILE_FILE", "/logs/w.profraw")
        .env_remove("ORBIT_ROOT");

    let scoped = scoped_worker_command(&base, "orbit-worker-jrun-1-0000abcd.scope", &limits());

    assert_eq!(scoped.get_program(), OsStr::new("systemd-run"));
    assert_eq!(
        args(&scoped),
        vec![
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--unit=orbit-worker-jrun-1-0000abcd.scope",
            "--property=MemoryHigh=6G",
            "--property=MemoryMax=8G",
            "--property=TasksMax=512",
            "--property=OOMPolicy=continue",
            "--",
            "/opt/orbit/bin/orbit",
            "job",
            "run-pipeline-worker",
            "jrun-1",
        ]
    );
    assert_eq!(scoped.get_current_dir(), Some(Path::new("/work/repo")));
    let envs = scoped.get_envs().collect::<Vec<_>>();
    assert!(envs.contains(&(
        OsStr::new("LLVM_PROFILE_FILE"),
        Some(OsStr::new("/logs/w.profraw"))
    )));
    assert!(envs.contains(&(OsStr::new("ORBIT_ROOT"), None)));
}

#[test]
fn unit_names_are_valid_and_unique_per_launch() {
    let first = scope_unit_name("jrun-20260923-0308-c3");
    let second = scope_unit_name("jrun-20260923-0308-c3");
    assert!(first.starts_with("orbit-worker-jrun-20260923-0308-c3-"));
    assert!(first.ends_with(".scope"));
    assert_ne!(first, second, "a duplicate delivery needs its own unit");

    let odd = scope_unit_name("run/with spaces@x");
    assert!(odd.starts_with("orbit-worker-run_with_spaces_x-"));
    assert!(
        odd.trim_end_matches(".scope")
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
    );
}

#[test]
fn only_a_worker_scope_cgroup_is_trusted() {
    let root = Path::new("/sys/fs/cgroup");
    let scope = WorkerScopeCgroup::from_proc_cgroup(
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/orbit-worker-jrun-1-0000abcd.scope\n",
        root,
    )
    .expect("worker scope is recognized");
    assert_eq!(
        scope.directory(),
        Path::new(
            "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/orbit-worker-jrun-1-0000abcd.scope"
        )
    );

    // A worker that fell back to its launcher's cgroup must not read that
    // service's counters as its own.
    for foreign in [
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/orbit-web.service\n",
        "0::/\n",
        "1:name=systemd:/orbit-worker-jrun-1.scope\n",
    ] {
        assert_eq!(WorkerScopeCgroup::from_proc_cgroup(foreign, root), None);
    }
}

#[test]
fn limit_breach_reports_oom_kills_and_refused_forks() {
    let root = TempDir::new().expect("tempdir");
    let path = "/app.slice/orbit-worker-jrun-1-0000abcd.scope";
    let directory = root.path().join(path.trim_start_matches('/'));
    std::fs::create_dir_all(&directory).expect("scope dir");
    let scope = WorkerScopeCgroup::from_proc_cgroup(&format!("0::{path}\n"), root.path())
        .expect("worker scope");

    // No counters (or a cgroup already removed) is not a breach.
    assert_eq!(scope.limit_breach(), None);
    std::fs::write(
        directory.join("memory.events"),
        "low 0\nhigh 12\nmax 3\noom 1\noom_kill 0\noom_group_kill 0\n",
    )
    .expect("memory.events");
    std::fs::write(directory.join("pids.events"), "max 0\n").expect("pids.events");
    assert_eq!(
        scope.limit_breach(),
        None,
        "throttling at MemoryHigh is not a failure cause"
    );

    std::fs::write(
        directory.join("memory.events"),
        "low 0\nhigh 12\nmax 3\noom 2\noom_kill 2\noom_group_kill 0\n",
    )
    .expect("memory.events");
    std::fs::write(directory.join("pids.events"), "max 57\n").expect("pids.events");
    std::fs::write(directory.join("memory.max"), "8589934592\n").expect("memory.max");
    std::fs::write(directory.join("pids.max"), "512\n").expect("pids.max");

    let description = scope.limit_breach().expect("breach").describe();
    assert!(description.contains("orbit-worker-jrun-1-0000abcd.scope"));
    assert!(description.contains("OOM-killed 2 process(es)"));
    assert!(description.contains("memory.max=8589934592"));
    assert!(description.contains("57 fork/clone attempt(s)"));
    assert!(description.contains("pids.max=512"));
}

#[test]
fn event_counts_are_read_by_exact_key() {
    let events = "oom 4\noom_kill 3\noom_group_kill 1\n";
    assert_eq!(event_count(events, "oom_kill"), 3);
    assert_eq!(event_count(events, "oom"), 4);
    assert_eq!(event_count(events, "max"), 0);
    assert_eq!(event_count("max garbage\n", "max"), 0);
}
