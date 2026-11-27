use super::*;

fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A dependency that reached `done` before its commit reached the base holds
/// its dependent in the backlog as `dependency_not_delivered` instead of
/// dispatching it into a `worktree_setup` failure on every pass; the pass
/// admits it once the commit is on the base.
#[test]
fn a_dependent_waits_in_backlog_until_its_done_dependency_reaches_the_base() {
    if !isolated(
        "dispatch_admission::dependency_delivery::a_dependent_waits_in_backlog_until_its_done_dependency_reaches_the_base",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    git(&repo, &["init"]);
    git(&repo, &["commit", "--allow-empty", "-m", "chore: base"]);
    let dependency = seed(
        &runtime,
        Seed::named("done dependency").status(TaskStatus::Done),
    );
    git(&repo, &["checkout", "-b", "dependency"]);
    std::fs::write(repo.join("dependency-payload"), "dependency\n").unwrap();
    git(&repo, &["add", "dependency-payload"]);
    git(
        &repo,
        &[
            "commit",
            "-m",
            &format!("feat: dependency [{}]", dependency.id),
        ],
    );
    git(&repo, &["checkout", "main"]);
    let dependent = seed(
        &runtime,
        Seed::named("dependent").depends(vec![dependency.id.clone()]),
    );

    let output = list_backlog_tasks(&runtime, json!({}));
    assert!(!admitted(&output).contains(&dependent.id), "{output:#}");
    let entry = excluded_entry(&output, &dependent.id);
    assert_eq!(entry["reason"], "dependency_not_delivered", "{entry}");
    assert!(
        entry["detail"].as_str().unwrap().contains(&dependency.id),
        "{entry}"
    );
    let readiness = default_readiness(&runtime);
    let task = readiness_task(&readiness, &dependent.id);
    assert_eq!(task["eligible"], false, "{task}");
    assert_eq!(task["reason"], "dependency_not_delivered", "{task}");

    git(
        &repo,
        &["merge", "--no-ff", "-m", "merge dependency", "dependency"],
    );
    let output = list_backlog_tasks(&runtime, json!({}));
    assert!(admitted(&output).contains(&dependent.id), "{output:#}");
}
