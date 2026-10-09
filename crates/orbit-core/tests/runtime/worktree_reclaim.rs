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
