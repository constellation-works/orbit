use super::*;
use orbit_engine::RunLifecycle;
use orbit_types::task::{TaskRelation, TaskRelationType};
use orbit_types::workflow::DeliveryRefusal;

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
                let Err(OrbitError::DeliveryRefused {
                    refusal: DeliveryRefusal::DependencyNotDelivered,
                    message,
                }) = refused
                else {
                    panic!("{case}: quote-only base must hold dependency admission: {refused:?}");
                };
                assert!(message.contains("ORB-222"), "{case}: {message}");
                assert!(message.contains(&base), "{case}: {message}");
                assert!(message.contains(&dependency), "{case}: {message}");
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

/// Comma-list prose and an unclosed bracket are not delivery. A real `[ID]`,
/// an older `[A, B]` list, and an `Orbit-Task` trailer are.
///
/// The pre-fix matcher treated `ORB-111, ORB-222, ORB-333` outside brackets,
/// and a comma-list after a stray `[` with no closing `]`, as delivery into
/// the base. `worktree_setup` then admitted the dependent run.
#[test]
fn worktree_setup_refuses_prose_dependency_mentions_and_accepts_delivery_markers() {
    isolated_in(
        module_path!(),
        "worktree_setup_refuses_prose_dependency_mentions_and_accepts_delivery_markers",
        || {
            let prose = "feat: discussed work\n\nRelated work: ORB-111, ORB-222, ORB-333 were discussed, none delivered here.";
            let unclosed = "fix: parser [ORB-111, ORB-222, ORB-333 handled; see [docs]";
            // `on_base` is where `message` belongs. Refusal cases commit that
            // message only on the base and keep `feat: delivered [ORB-222]`
            // on `dependency`. Acceptance cases cherry-pick `message` onto a
            // base that already contains both false positives.
            let cases = [
                ("prose", prose, false),
                ("unclosed", unclosed, false),
                ("single", "feat: single [ORB-222]", true),
                ("list", "feat: list [ORB-111, ORB-222]", true),
                (
                    "trailer",
                    "feat: trailer delivery\n\nBody.\n\nOrbit-Task: ORB-222",
                    true,
                ),
            ];
            for (case, message, on_base) in cases {
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
                let delivery = if on_base {
                    message
                } else {
                    "feat: delivered [ORB-222]"
                };
                git(repo, &["commit", "-m", delivery]);
                let dependency = head(repo);
                git(repo, &["checkout", BASE]);

                if on_base {
                    // False positives already on the base must not hide the
                    // real marker, and must not be required for admission.
                    git(repo, &["commit", "--allow-empty", "-m", prose]);
                    git(repo, &["commit", "--allow-empty", "-m", unclosed]);
                    git(repo, &["cherry-pick", &dependency]);
                } else {
                    git(repo, &["commit", "--allow-empty", "-m", message]);
                    assert!(
                        !is_ancestor(repo, &dependency, &head(repo)),
                        "{case}: delivery must stay off the base"
                    );
                    assert!(
                        !repo.join("dependency-payload").exists(),
                        "{case}: base must not contain the other branch's delivery"
                    );
                }
                let base = head(repo);
                let worktrees = registered_worktrees(repo);
                let branches = git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]);
                let run_id = format!("jrun-marker-boundary-{case}");
                let mut input = setup_input(&["ORB-333"], &run_id);
                input.as_object_mut().unwrap().remove("dependency_delivery");

                let outcome = action(&host, "worktree_setup", &input);
                if on_base {
                    let setup = outcome.unwrap_or_else(|error| {
                        panic!("{case}: delivery marker on the base must admit the run: {error}")
                    });
                    assert_eq!(setup["base_sha"], base, "{case}");
                    assert_eq!(read(workspace(&setup).join("dependency-payload")), case);
                    assert_eq!(
                        host.get_task("ORB-333").unwrap().status,
                        TaskStatus::InProgress,
                        "{case}"
                    );
                } else {
                    let Err(OrbitError::DeliveryRefused {
                        refusal: DeliveryRefusal::DependencyNotDelivered,
                        message,
                    }) = outcome
                    else {
                        panic!(
                            "{case}: a base that only mentions the dependency must hold: {outcome:?}"
                        );
                    };
                    assert!(message.contains("ORB-222"), "{case}: {message}");
                    assert!(
                        message.contains(&dependency),
                        "{case}: hold must name the delivery that is elsewhere: {message}"
                    );
                    assert_eq!(
                        host.get_task("ORB-333").unwrap().status,
                        TaskStatus::Backlog,
                        "{case}"
                    );
                    assert!(host.admitted.lock().unwrap().is_empty(), "{case}");
                    assert_eq!(registered_worktrees(repo), worktrees, "{case}");
                    assert_eq!(
                        git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]),
                        branches,
                        "{case}"
                    );
                }
            }
        },
    );
}

/// A backlog task whose done dependency has commits only on a non-base ref is
/// held, leaving the task, worktrees and branches as they were. A task a
/// previous run already admitted (`in-progress`) keeps failing with the typed
/// dependency refusal, because a held delivery refusal has no generic resume
/// path for it.
#[test]
fn worktree_setup_holds_backlog_but_fails_in_progress_on_undelivered_dependency() {
    isolated_in(
        module_path!(),
        "worktree_setup_holds_backlog_but_fails_in_progress_on_undelivered_dependency",
        || {
            for (case, status) in [
                ("backlog", TaskStatus::Backlog),
                ("in_progress", TaskStatus::InProgress),
            ] {
                let (fixture, host) = Fixture::hosted();
                let repo = &fixture.repo;
                host.add_task("ORB-222", TaskStatus::Done);
                host.add_task("ORB-333", status);
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
                git(repo, &["commit", "-m", "feat: dependency [ORB-222]"]);
                let dependency = head(repo);
                git(repo, &["checkout", BASE]);
                let base = head(repo);
                let worktrees = registered_worktrees(repo);
                let branches = git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]);
                let mut input = setup_input(&["ORB-333"], &format!("jrun-hold-{case}"));
                input.as_object_mut().unwrap().remove("dependency_delivery");

                let outcome = action(&host, "worktree_setup", &input);
                match status {
                    TaskStatus::Backlog => {
                        let Err(OrbitError::DeliveryRefused { refusal, message }) = outcome else {
                            panic!("{case}: a backlog task must be held: {outcome:?}");
                        };
                        assert_eq!(refusal, DeliveryRefusal::DependencyNotDelivered);
                        assert!(message.contains(&dependency), "{case}: {message}");
                    }
                    _ => {
                        let Err(OrbitError::DependencyNotDelivered(detail)) = outcome else {
                            panic!("{case}: an admitted task must keep failing: {outcome:?}");
                        };
                        assert_eq!(detail.dependency_id, "ORB-222", "{case}");
                        assert_eq!(detail.base_sha, base, "{case}");
                    }
                }
                assert_eq!(host.get_task("ORB-333").unwrap().status, status, "{case}");
                assert!(host.admitted.lock().unwrap().is_empty(), "{case}");
                assert_eq!(registered_worktrees(repo), worktrees, "{case}");
                assert_eq!(
                    git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]),
                    branches,
                    "{case}"
                );

                // Ignoring the check is the operator's explicit escape hatch.
                let ignored = setup_input(&["ORB-333"], &format!("jrun-hold-ignore-{case}"));
                action(&host, "worktree_setup", &ignored)
                    .unwrap_or_else(|error| panic!("{case}: ignore must admit: {error}"));
            }
        },
    );
}
