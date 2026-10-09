//! Disk-pressure recovery through actual backlog admission, over real Git worktrees.

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn admission_reclaims_oldest_kept_worktrees_first_and_leaves_active_runs() {
    if !super::dispatch_admission::isolated(
        "worktree_reclaim::admission_reclaims_oldest_kept_worktrees_first_and_leaves_active_runs",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    let global = root.path().join("global");
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(
        repo.join(".orbit/config.toml"),
        "[worktree]\nreclaim = ['cache']\nreclaim_below_free_mib = 1099511627776\n",
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "base"]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let mut kept = Vec::new();
    // Insert in reverse age order, so insertion order cannot pass this test.
    for (age, state) in [
        (1, JobRunState::Failed),
        (2, JobRunState::Failed),
        (3, JobRunState::Running),
    ] {
        let at = Utc::now() - Duration::hours(age);
        let run = jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                at,
                Some(json!({"task_ids": [format!("T-AGE-{age}")]})),
                None,
            )
            .unwrap();
        let worktree = repo
            .join(".orbit/state/worktrees")
            .join(format!("orbit-{}", run.run_id));
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-qb",
                &format!("orbit/{}", run.run_id),
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(worktree.join("cache")).unwrap();
        std::fs::write(worktree.join("cache/output"), vec![0; 37]).unwrap();
        if state.is_terminal() {
            let mut worker = std::process::Command::new("true").spawn().unwrap();
            let pid = worker.id();
            worker.wait().unwrap();
            jobs.mark_job_run_running(&run.run_id, at, pid).unwrap();
            jobs.finalize_job_run(&run.run_id, state, at, None).unwrap();
        } else {
            jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
                .unwrap();
        }
        kept.push((run.run_id, worktree));
    }
    // Inspection and readiness remain observational, even with the threshold on.
    assert_eq!(runtime.reclaimable_worktrees().unwrap().bytes_reclaimed, 74);
    assert!(
        kept.iter()
            .all(|(_, path)| path.join("cache/output").exists())
    );
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(!kept[0].1.join("cache").exists());
    assert!(!kept[1].1.join("cache").exists());
    assert!(kept[2].1.join("cache/output").exists());
    assert!(kept.iter().all(|(_, path)| path.join(".git").exists()));
    let mut events = runtime
        .list_audit_events(None, Some("worktree.reclaim".into()), None, None, 100)
        .unwrap();
    events.sort_by_key(|event| event.timestamp);
    let order = events
        .iter()
        .map(|event| {
            let value: Value =
                serde_json::from_str(event.arguments_json.as_deref().unwrap()).unwrap();
            value["run_id"].as_str().unwrap().to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [kept[1].0.clone(), kept[0].0.clone()],
        "oldest output must be reclaimed first"
    );
}

#[cfg(unix)]
#[test]
fn admission_skips_empty_terminal_history_without_git_and_detects_new_output() {
    use std::os::unix::fs::PermissionsExt;

    if !super::dispatch_admission::isolated(
        "worktree_reclaim::admission_skips_empty_terminal_history_without_git_and_detects_new_output",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    let global = root.path().join("global");
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    let threshold = 1_099_511_627_776_u64;
    std::fs::write(
        repo.join(".orbit/config.toml"),
        format!("[worktree]\nreclaim = ['cache']\nreclaim_below_free_mib = {threshold}\n"),
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "base"]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let mut worker = std::process::Command::new("true").spawn().unwrap();
    let pid = worker.id();
    worker.wait().unwrap();
    let mut kept = Vec::new();
    for age in 0..128 {
        let at = Utc::now() - Duration::hours(age + 1);
        let run = jobs
            .insert_job_run("task_pr_pipeline", 1, at, None, None)
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, at, pid).unwrap();
        jobs.finalize_job_run(&run.run_id, JobRunState::Failed, at, None)
            .unwrap();
        // Include both real retained checkouts without output and runs whose
        // worktrees are gone. Neither should trigger a collection.
        if age < 8 {
            let worktree = orbit_engine::run_worktree_paths(&repo, &run)
                .unwrap()
                .remove(0);
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "-q",
                    worktree.to_str().unwrap(),
                ],
            );
            std::fs::create_dir_all(worktree.join("sources/nested")).unwrap();
            std::fs::write(worktree.join("sources/nested/keep"), "source").unwrap();
            kept.push(worktree);
        }
    }

    let real_git = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let bin = root.path().join("bin");
    let calls = root.path().join("git-calls");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(&calls, "").unwrap();
    std::fs::write(
        bin.join("git"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            calls.display(),
            real_git.trim(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    // This test owns an isolated, single-test child process.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    let admit = || {
        assert!(fs2::available_space(repo.join(".orbit")).unwrap() / (1024 * 1024) < threshold);
        runtime
            .run_deterministic(
                "list_backlog_tasks",
                &json!({}),
                &json!({}),
                ToolContext::default(),
            )
            .unwrap();
    };
    for _ in 0..2 {
        admit();
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "",
            "empty terminal history must not spawn Git while disk stays below the threshold"
        );
    }
    assert!(
        runtime
            .list_audit_events(None, Some("worktree.reclaim".into()), None, None, 100)
            .unwrap()
            .is_empty()
    );

    // Positive control: the observer must see real collector calls, and a
    // prior empty pass must not hide output created before a later admission.
    std::fs::create_dir_all(kept[0].join("cache")).unwrap();
    std::fs::write(kept[0].join("cache/output"), vec![0; 37]).unwrap();
    admit();
    assert!(
        !kept[0].join("cache").exists(),
        "new output was retained: {:?}; Git calls: {}",
        runtime.reclaimable_worktrees().unwrap(),
        std::fs::read_to_string(&calls).unwrap()
    );
    assert!(!std::fs::read_to_string(&calls).unwrap().is_empty());
    std::fs::write(&calls, "").unwrap();
    admit();
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "");
    assert!(kept.iter().all(|path| path.join(".git").exists()));
    assert!(
        kept.iter()
            .all(|path| path.join("sources/nested/keep").exists())
    );
}
