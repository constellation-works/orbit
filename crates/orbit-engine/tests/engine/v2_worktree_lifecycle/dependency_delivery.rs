use super::*;
use orbit_types::task::{TaskRelation, TaskRelationType};

#[test]
fn worktree_setup_requires_owned_dependency_delivery_beneath_fixes_quotes() {
    isolated_in(
        module_path!(),
        "worktree_setup_requires_owned_dependency_delivery_beneath_fixes_quotes",
        || {
            for (case, delivery) in [
                ("single", "feat: dependency [ORB-222]"),
                ("list", "feat: bundle [ORB-111, ORB-222]"),
                (
                    "body",
                    "feat: dependency\n\nDelivered [ORB-222] in this body.",
                ),
                ("trailer", "feat: dependency\n\nOrbit-Task: ORB-222"),
            ] {
                let (fixture, host) = Fixture::hosted();
                let repo = &fixture.repo;
                host.add_task("ORB-222", TaskStatus::Done);
                host.add_task("ORB-333", TaskStatus::Backlog);
                host.edit_task("ORB-333", |task| {
                    task.relations.push(TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: "ORB-222".to_string(),
                        regression: None,
                    });
                });

                git(repo, &["checkout", "-b", "dependency"]);
                fs::write(repo.join("dependency-payload"), case).unwrap();
                git(repo, &["add", "dependency-payload"]);
                git(repo, &["commit", "-m", delivery]);
                let dependency = head(repo);
                git(repo, &["checkout", BASE]);

                let quote = format!(
                    "fix: unrelated repair [ORB-444]\n\nOrbit-Task: ORB-444\n\n  Fixes: {dependency} (\"feat: actual dependency [ORB-222]\")"
                );
                // More quotes than the evidence limit must not hide the real
                // delivery on the side branch when searching all refs either.
                for _ in 0..4 {
                    git(repo, &["commit", "--allow-empty", "-m", &quote]);
                }
                let base = head(repo);
                assert!(!repo.join("dependency-payload").exists(), "{case}");
                assert!(!is_ancestor(repo, &dependency, &base), "{case}");
                let worktrees = registered_worktrees(repo);
                let branches = git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]);
                let run_id = format!("jrun-owned-delivery-{case}");
                let mut input = setup_input(&["ORB-333"], &run_id);
                input.as_object_mut().unwrap().remove("dependency_delivery");

                let refused = action(&host, "worktree_setup", &input);
                let Err(OrbitError::DependencyNotDelivered(detail)) = refused else {
                    panic!("{case}: quote-only base must refuse dependency admission: {refused:?}");
                };
                assert_eq!(detail.dependency_id, "ORB-222", "{case}");
                assert_eq!(detail.base_sha, base, "{case}");
                assert!(detail.detail.contains(&dependency), "{case}: {detail:?}");
                assert_eq!(
                    host.get_task("ORB-333").unwrap().status,
                    TaskStatus::Backlog
                );
                assert!(host.admitted.lock().unwrap().is_empty(), "{case}");
                assert_eq!(registered_worktrees(repo), worktrees, "{case}");
                assert_eq!(
                    git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]),
                    branches,
                    "{case}"
                );

                git(repo, &["cherry-pick", &dependency]);
                git(repo, &["commit", "--allow-empty", "-m", &quote]);
                let delivered_base = head(repo);
                let setup = action(&host, "worktree_setup", &input).unwrap_or_else(|error| {
                    panic!("{case}: owned delivery beneath quote: {error}")
                });
                assert_eq!(setup["base_sha"], delivered_base, "{case}");
                assert_eq!(read(workspace(&setup).join("dependency-payload")), case);
                assert_eq!(
                    host.get_task("ORB-333").unwrap().status,
                    TaskStatus::InProgress
                );
            }
        },
    );
}
