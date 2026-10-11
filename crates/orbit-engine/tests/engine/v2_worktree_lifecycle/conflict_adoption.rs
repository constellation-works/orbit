//! [F2026-10-237] Conflict files repaired before `pr_conflict_recovery` runs.
//!
//! Final recovery repairs a stopped rebase's conflict files without Git
//! writes and resumes; the retried `sync_base` reports the same stop and the
//! next conflict recovery has nothing left to change. The host adopts those
//! earlier repairs, with the unstaged tracked companions made beside them,
//! and continues the existing rebase. Each test plays the earlier actor by
//! editing the checkout before a no-op substitute provider runs. Adoption
//! never loosens the boundary: markers, whitespace errors, untouched
//! conflicts, moved HEAD or index, a checkpoint that does not describe the
//! stopped rebase, and changed ownership or authorization all still refuse.
//! Binary ancestors and failing custom drivers can leave marker-free stops;
//! a no-op provider must preserve their unmerged index until a real repair.

use super::*;

/// A provider that changes nothing and reports success.
fn no_op_provider(prepared: &PreparedRebase) -> PathBuf {
    let provider = prepared.fixture.root.path().join("codex");
    write_executable(&provider, &provider_script(""));
    provider
}

/// Commit `edits` in `repo` as one commit: `Some` writes a file, `None`
/// removes it.
fn commit_edits(repo: &Path, edits: &[(&str, Option<&str>)]) -> String {
    for (file, contents) in edits {
        match contents {
            Some(contents) => {
                fs::write(repo.join(file), contents).unwrap();
                git(repo, &["add", "--", file]);
            }
            None => {
                git(repo, &["rm", "-q", "--", file]);
            }
        }
    }
    git(repo, &["commit", "-m", "edits"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// A one-commit candidate applying `candidate`, stopped on its rebase onto a
/// base advanced by one commit applying `base`.
fn stopped_on(
    run_id: &str,
    candidate: &[(&str, Option<&str>)],
    base: &[(&str, Option<&str>)],
) -> (PreparedRebase, Value) {
    let prepared = PreparedRebase::with_edits(
        run_id,
        |checkout| commit_edits(checkout, candidate),
        |repo| commit_edits(repo, base),
    );
    let conflict = stopped_conflict(&prepared);
    (prepared, conflict)
}

/// The rebase is still stopped, nothing is certified or widened, and the
/// README conflict file keeps the bytes the earlier actor left.
fn assert_refused_untouched(
    case: &str,
    prepared: &PreparedRebase,
    host: &LifecycleHost,
    readme: &str,
) {
    assert!(rebase_in_progress(&prepared.checkout.path), "{case}");
    assert!(host.checkpoints().is_empty(), "{case}");
    assert!(host.widenings().is_empty(), "{case}");
    assert_eq!(
        fs::read_to_string(prepared.checkout.path.join("README.md")).unwrap(),
        readme,
        "{case}: the earlier repair stays for diagnosis"
    );
}

/// Seed the common ancestor before setup, then stop on two textual sides.
/// A custom driver may be selected by attribute or by `merge.default`.
fn marker_free_stop(
    run_id: &str,
    ancestor: &str,
    driver: Option<&str>,
    attribute: bool,
) -> (PreparedRebase, Value) {
    let fixture = Fixture::new();
    if fs::read(fixture.repo.join("README.md")).unwrap() != ancestor.as_bytes() {
        commit_edits(&fixture.repo, &[("README.md", Some(ancestor))]);
    }
    if let Some(driver) = driver {
        git(
            &fixture.repo,
            &["config", "merge.leave-ours.driver", driver],
        );
        if attribute {
            commit_edits(
                &fixture.repo,
                &[(".gitattributes", Some("README.md merge=leave-ours\n"))],
            );
        } else {
            git(&fixture.repo, &["config", "merge.default", "leave-ours"]);
        }
    }
    let prepared = PreparedRebase::with_fixture(
        run_id,
        fixture,
        |checkout| commit_edits(checkout, &[("README.md", Some("candidate\n"))]),
        |repo| commit_edits(repo, &[("README.md", Some("target\n"))]),
    );
    let conflict = stopped_conflict(&prepared);
    assert_eq!(conflict["conflicting_paths"], json!(["README.md"]));
    (prepared, conflict)
}

/// No-op recovery cannot certify Git's untouched marker-free output, even
/// when a failing driver writes content different from either side.
#[cfg(unix)]
#[test]
fn untouched_binary_ancestor_and_custom_driver_stops_are_not_adopted() {
    isolated_in(
        module_path!(),
        "untouched_binary_ancestor_and_custom_driver_stops_are_not_adopted",
        || {
            for (case, ancestor, driver, attribute, stopped) in [
                ("binary-ancestor", "base\0data\n", None, false, "target\n"),
                ("custom-driver", "base\n", Some("false"), true, "target\n"),
                ("default-driver", "base\n", Some("false"), false, "target\n"),
                (
                    "custom-output",
                    "base\n",
                    Some("printf 'driver output\\n' > %A; false"),
                    true,
                    "driver output\n",
                ),
            ] {
                let (prepared, conflict) =
                    marker_free_stop(&format!("jrun-{case}"), ancestor, driver, attribute);
                let checkout = &prepared.checkout.path;
                assert_eq!(
                    fs::read_to_string(checkout.join("README.md")).unwrap(),
                    stopped
                );
                assert_eq!(git(checkout, &["show", ":1:README.md"]), ancestor.trim());
                assert_eq!(git(checkout, &["show", ":2:README.md"]), "target");
                assert_eq!(git(checkout, &["show", ":3:README.md"]), "candidate");
                git(checkout, &["diff", "--check"]);
                let unmerged = git(checkout, &["ls-files", "--unmerged"]);
                if case == "binary-ancestor" {
                    // Exercise the fallback for strategies without a stopped tree.
                    git(checkout, &["update-ref", "-d", "AUTO_MERGE"]);
                }
                let host = prepared.host.with_provider(&no_op_provider(&prepared));
                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("untouched stopped output must not resolve the conflict");
                assert!(
                    error
                        .to_string()
                        .contains("authorized conflict files were not repaired"),
                    "{case}: {error}"
                );
                assert_refused_untouched(case, &prepared, &host, stopped);
                assert_eq!(
                    git(checkout, &["ls-files", "--unmerged"]),
                    unmerged,
                    "{case}: all ancestor and side entries remain unstaged"
                );
            }
        },
    );
}

/// Real file-only repairs of marker-free stops continue at the host boundary.
/// A text repair may still take one side verbatim, and custom-driver repairs
/// are distinguished from the actual stopped output rather than either side.
#[cfg(unix)]
#[test]
fn earlier_repairs_of_binary_ancestor_and_custom_driver_stops_continue() {
    isolated_in(
        module_path!(),
        "earlier_repairs_of_binary_ancestor_and_custom_driver_stops_continue",
        || {
            for (case, ancestor, driver, attribute, repair) in [
                (
                    "binary-repair",
                    "base\0data\n",
                    None,
                    false,
                    "candidate and target\n",
                ),
                (
                    "custom-repair",
                    "base\n",
                    Some("false"),
                    true,
                    "candidate\n",
                ),
                (
                    "default-repair",
                    "base\n",
                    Some("false"),
                    false,
                    "candidate and target\n",
                ),
                ("text-side-repair", "base\n", None, false, "candidate\n"),
                (
                    "custom-output-repair",
                    "base\n",
                    Some("printf 'driver output\\n' > %A; false"),
                    true,
                    "candidate and target\n",
                ),
            ] {
                let (prepared, conflict) =
                    marker_free_stop(&format!("jrun-{case}"), ancestor, driver, attribute);
                let checkout = &prepared.checkout.path;
                fs::write(checkout.join("README.md"), repair).unwrap();
                if case == "text-side-repair" {
                    git(checkout, &["update-ref", "-d", "AUTO_MERGE"]);
                }
                let host = prepared.host.with_provider(&no_op_provider(&prepared));
                let outcome = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect("a real earlier repair continues the rebase");
                assert!(outcome.success, "{case}: {:?}", outcome.message);
                assert!(!rebase_in_progress(checkout), "{case}");
                assert!(is_ancestor(checkout, &prepared.target, "HEAD"), "{case}");
                assert_eq!(
                    git(checkout, &["show", "HEAD:README.md"]),
                    repair.trim(),
                    "{case}"
                );
                assert_eq!(git(checkout, &["status", "--porcelain"]), "", "{case}");
                assert_eq!(
                    host.checkpoints().len(),
                    1,
                    "{case}: one certified recovery"
                );
                let retried = prepared.rebase_on(&host, &prepared.prepared).unwrap();
                assert_eq!(retried["decision"], "reused_recovery", "{case}");
            }
        },
    );
}

/// Original stopped-output evidence must exist for custom drivers and stay
/// unchanged while the provider runs; neither case may stage the repair.
#[cfg(unix)]
#[test]
fn custom_driver_repairs_require_unchanged_stopped_output_evidence() {
    isolated_in(
        module_path!(),
        "custom_driver_repairs_require_unchanged_stopped_output_evidence",
        || {
            for (case, refusal) in [
                ("missing", "authorized conflict files were not repaired"),
                (
                    "missing-default",
                    "authorized conflict files were not repaired",
                ),
                (
                    "changed",
                    "changed Git metadata identity or recovery instructions",
                ),
            ] {
                let (prepared, conflict) = marker_free_stop(
                    &format!("jrun-evidence-{case}"),
                    "base\n",
                    Some("false"),
                    case != "missing-default",
                );
                let checkout = &prepared.checkout.path;
                fs::write(checkout.join("README.md"), "candidate and target\n").unwrap();
                let unmerged = git(checkout, &["ls-files", "--unmerged"]);
                let provider = if case.starts_with("missing") {
                    git(checkout, &["update-ref", "-d", "AUTO_MERGE"]);
                    no_op_provider(&prepared)
                } else {
                    let provider = prepared.fixture.root.path().join("codex");
                    write_executable(&provider, &provider_script("git update-ref -d AUTO_MERGE"));
                    provider
                };
                let host = prepared.host.with_provider(&provider);
                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("custom-driver adoption requires original stopped-output evidence");
                assert!(error.to_string().contains(refusal), "{case}: {error}");
                assert_refused_untouched(case, &prepared, &host, "candidate and target\n");
                assert_eq!(
                    git(checkout, &["ls-files", "--unmerged"]),
                    unmerged,
                    "{case}"
                );
            }
        },
    );
}

/// A rebase stopped on a content conflict and a modify/delete conflict, both
/// repaired before the invocation (one rewritten, one deleted), continues
/// through a no-op recovery. The continued commit holds the conflict set and
/// the unstaged tracked companions the earlier actor made; the untracked
/// payload it left and its `.orbit/` scratch stay out, and the retry delivers
/// the certified rewrite.
#[cfg(unix)]
#[test]
fn a_no_op_recovery_continues_the_rebase_from_earlier_repairs() {
    isolated_in(
        module_path!(),
        "a_no_op_recovery_continues_the_rebase_from_earlier_repairs",
        || {
            let (prepared, conflict) = stopped_on(
                "jrun-adopt",
                &[
                    ("README.md", Some("candidate\n")),
                    ("base.txt", Some("candidate v1\n")),
                    ("lib.txt", Some("lib\n")),
                ],
                &[
                    ("README.md", Some("target\n")),
                    ("base.txt", None),
                    ("other.txt", Some("other\n")),
                ],
            );
            assert_eq!(
                conflict["conflicting_paths"],
                json!(["README.md", "base.txt"])
            );
            let checkout = &prepared.checkout.path;
            let target = prepared.target.clone();

            // Final recovery's file-only repairs.
            fs::write(checkout.join("README.md"), "candidate and target\n").unwrap();
            fs::remove_file(checkout.join("base.txt")).unwrap();
            fs::write(checkout.join("lib.txt"), "lib repaired\n").unwrap();
            fs::write(checkout.join("other.txt"), "other repaired\n").unwrap();
            fs::write(checkout.join("payload.txt"), "untracked payload\n").unwrap();
            fs::create_dir_all(checkout.join(".orbit/tmp")).unwrap();
            fs::write(checkout.join(".orbit/tmp/repairs.json"), "{}\n").unwrap();

            let host = prepared.host.with_provider(&no_op_provider(&prepared));
            let outcome = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect("earlier repairs continue the stopped rebase");
            assert!(outcome.success, "{:?}", outcome.message);
            assert!(!rebase_in_progress(checkout));
            assert!(is_ancestor(checkout, &target, "HEAD"));
            assert_eq!(
                git(
                    checkout,
                    &["show", "--format=", "--name-only", "--no-renames", "HEAD"]
                ),
                "README.md\nlib.txt\nother.txt",
                "the continued commit holds the conflict set and the earlier companions"
            );
            assert_eq!(
                git(checkout, &["show", "HEAD:README.md"]),
                "candidate and target"
            );
            assert_eq!(git(checkout, &["show", "HEAD:lib.txt"]), "lib repaired");
            assert_eq!(git(checkout, &["show", "HEAD:other.txt"]), "other repaired");
            assert!(
                git(checkout, &["ls-tree", "--name-only", "HEAD"])
                    .lines()
                    .all(|path| path != "base.txt"),
                "the deletion resolved the modify/delete conflict"
            );
            assert_eq!(
                git(checkout, &["status", "--porcelain"]),
                "?? payload.txt",
                "only the untracked payload remains, untracked and unchanged"
            );
            assert_eq!(
                fs::read_to_string(checkout.join("payload.txt")).unwrap(),
                "untracked payload\n"
            );
            assert_eq!(
                fs::read_to_string(checkout.join(".orbit/tmp/repairs.json")).unwrap(),
                "{}\n"
            );

            let head = prepared.head();
            let checkpoints = host.checkpoints();
            let [(_, step_id, checkpoint)] = checkpoints.as_slice() else {
                panic!("expected one recovery checkpoint, got {checkpoints:#?}");
            };
            assert_eq!(step_id, "sync_base");
            assert_eq!(checkpoint["head_sha"], head);
            assert_eq!(checkpoint["base_sha"], target);
            assert_eq!(
                checkpoint["companion_paths"],
                json!(["lib.txt", "other.txt"])
            );

            let retried = prepared
                .rebase_on(&host, &prepared.prepared)
                .expect("the retry delivers the continued rewrite");
            assert_eq!(retried["decision"], "reused_recovery");
            assert_eq!(retried["head_sha"], head);
        },
    );
}

/// Adoption needs a real repair. An earlier edit that kept conflict
/// markers, a conflict nobody touched (Git's markers, or the one side Git
/// left for a modify/delete), or one of two conflicts left untouched is
/// refused as unrepaired; an earlier repair with whitespace errors fails
/// `git diff --check`.
#[cfg(unix)]
#[test]
fn earlier_repairs_still_refuse_markers_whitespace_and_unrepaired_conflicts() {
    isolated_in(
        module_path!(),
        "earlier_repairs_still_refuse_markers_whitespace_and_unrepaired_conflicts",
        || {
            for (run_id, repair, refusal) in [
                (
                    "jrun-kept-markers",
                    Some("<<<<<<< HEAD\ntarget\n=======\ncandidate\n>>>>>>> candidate\n"),
                    "authorized conflict files were not repaired",
                ),
                (
                    "jrun-untouched",
                    None,
                    "authorized conflict files were not repaired",
                ),
                (
                    "jrun-whitespace",
                    Some("candidate and target  \n"),
                    "still contains conflict markers or whitespace errors",
                ),
            ] {
                let prepared = PreparedRebase::new(run_id, "README.md", "README.md");
                let conflict = stopped_conflict(&prepared);
                let readme = prepared.checkout.path.join("README.md");
                if let Some(repair) = repair {
                    fs::write(&readme, repair).unwrap();
                }
                let left = fs::read_to_string(&readme).unwrap();
                let host = prepared.host.with_provider(&no_op_provider(&prepared));

                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("no real repair refuses continuation");
                assert!(error.to_string().contains(refusal), "{run_id}: {error}");
                assert_refused_untouched(run_id, &prepared, &host, &left);
            }

            // Git leaves the modified side of a modify/delete conflict in
            // place, with no markers: still unrepaired.
            let (prepared, conflict) = stopped_on(
                "jrun-modify-delete",
                &[("base.txt", Some("candidate v1\n"))],
                &[("base.txt", None)],
            );
            assert_eq!(conflict["conflicting_paths"], json!(["base.txt"]));
            let host = prepared.host.with_provider(&no_op_provider(&prepared));
            let error = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect_err("an untouched modify/delete conflict refuses continuation");
            assert!(
                error
                    .to_string()
                    .contains("authorized conflict files were not repaired"),
                "{error}"
            );
            assert!(rebase_in_progress(&prepared.checkout.path));
            assert!(host.checkpoints().is_empty());
            assert_eq!(
                fs::read_to_string(prepared.checkout.path.join("base.txt")).unwrap(),
                "candidate v1\n",
                "Git's side stays in place"
            );

            // One of two conflicts repaired earlier, the other untouched.
            let (prepared, conflict) = stopped_on(
                "jrun-half-repaired",
                &[
                    ("README.md", Some("candidate\n")),
                    ("base.txt", Some("candidate v1\n")),
                ],
                &[
                    ("README.md", Some("target\n")),
                    ("base.txt", Some("target v1\n")),
                ],
            );
            assert_eq!(
                conflict["conflicting_paths"],
                json!(["README.md", "base.txt"])
            );
            fs::write(
                prepared.checkout.path.join("README.md"),
                "candidate and target\n",
            )
            .unwrap();
            let host = prepared.host.with_provider(&no_op_provider(&prepared));
            let error = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect_err("a conflict left untouched refuses continuation");
            assert!(
                error
                    .to_string()
                    .contains("authorized conflict files were not repaired"),
                "{error}"
            );
            assert_refused_untouched(
                "jrun-half-repaired",
                &prepared,
                &host,
                "candidate and target\n",
            );
        },
    );
}

/// Only file edits are adopted. An earlier actor that staged the repair,
/// re-attached HEAD to the branch, or moved the detached HEAD off the pinned
/// base is refused before the provider launches; a provider that stages a
/// companion beside earlier repairs is refused after it exits.
#[cfg(unix)]
#[test]
fn earlier_repairs_are_not_continued_after_head_branch_or_index_moved() {
    isolated_in(
        module_path!(),
        "earlier_repairs_are_not_continued_after_head_branch_or_index_moved",
        || {
            type Move = fn(&PreparedRebase);
            let cases: [(&str, Move); 3] = [
                ("jrun-staged-earlier", |prepared| {
                    git(&prepared.checkout.path, &["add", "README.md"]);
                }),
                ("jrun-attached-head", |prepared| {
                    let branch = prepared.prepared["head"].as_str().unwrap();
                    git(
                        &prepared.checkout.path,
                        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
                    );
                }),
                ("jrun-moved-head", |prepared| {
                    git(
                        &prepared.checkout.path,
                        &["update-ref", "--no-deref", "HEAD", &prepared.base_sha],
                    );
                }),
            ];
            for (run_id, earlier_move) in cases {
                let prepared = PreparedRebase::new(run_id, "README.md", "README.md");
                let conflict = stopped_conflict(&prepared);
                fs::write(
                    prepared.checkout.path.join("README.md"),
                    "candidate and target\n",
                )
                .unwrap();
                earlier_move(&prepared);
                let launched = prepared.fixture.root.path().join("provider-launched");
                let provider = prepared.fixture.root.path().join("codex");
                write_executable(
                    &provider,
                    &provider_script(&format!(": > '{}'", launched.display())),
                );
                let host = prepared.host.with_provider(&provider);

                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("an earlier Git move refuses admission");
                assert!(
                    error.to_string().contains("existing rebase matching"),
                    "{run_id}: {error}"
                );
                assert!(!launched.exists(), "{run_id}: the provider must not launch");
                assert_refused_untouched(run_id, &prepared, &host, "candidate and target\n");
            }

            let prepared = PreparedRebase::new("jrun-provider-staged", "README.md", "README.md");
            let conflict = stopped_conflict(&prepared);
            fs::write(
                prepared.checkout.path.join("README.md"),
                "candidate and target\n",
            )
            .unwrap();
            let provider = prepared.fixture.root.path().join("codex");
            write_executable(
                &provider,
                &provider_script("printf 'companion\\n' > companion.txt\ngit add companion.txt"),
            );
            let host = prepared.host.with_provider(&provider);
            let error = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect_err("a provider index change refuses continuation");
            assert!(
                error.to_string().contains("changed HEAD, branch, or index"),
                "{error}"
            );
            assert_refused_untouched(
                "jrun-provider-staged",
                &prepared,
                &host,
                "candidate and target\n",
            );
        },
    );
}

/// Earlier repairs never vouch for a checkpoint that does not describe the
/// stopped rebase: a wrong branch, original HEAD, pinned base, failed step
/// or conflict set is refused before the provider launches, and recovery
/// metadata rewritten while the provider ran is refused after it exits.
#[cfg(unix)]
#[test]
fn earlier_repairs_are_not_continued_for_a_mismatched_checkpoint() {
    isolated_in(
        module_path!(),
        "earlier_repairs_are_not_continued_for_a_mismatched_checkpoint",
        || {
            type Mismatch = fn(&PreparedRebase, &mut Value);
            let cases: [(&str, Mismatch); 5] = [
                ("jrun-wrong-branch", |_, input| {
                    input["failed_step_input"]["head"] = json!(BASE);
                }),
                ("jrun-wrong-original", |prepared, input| {
                    input["failed_step_input"]["head_sha"] = json!(prepared.base_sha);
                }),
                ("jrun-wrong-pin", |prepared, input| {
                    input["target_base_sha"] = json!(prepared.candidate);
                    input["failed_step_input"]["base_sha"] = json!(prepared.candidate);
                }),
                ("jrun-wrong-step", |_, input| {
                    input["failed_step_id"] = json!("complete_pr");
                }),
                ("jrun-wrong-conflicts", |_, input| {
                    input["conflicting_paths"] = json!(["README.md", "base.txt"]);
                }),
            ];
            for (run_id, mismatch) in cases {
                let prepared = PreparedRebase::new(run_id, "README.md", "README.md");
                let conflict = stopped_conflict(&prepared);
                fs::write(
                    prepared.checkout.path.join("README.md"),
                    "candidate and target\n",
                )
                .unwrap();
                let launched = prepared.fixture.root.path().join("provider-launched");
                let provider = prepared.fixture.root.path().join("codex");
                write_executable(
                    &provider,
                    &provider_script(&format!(": > '{}'", launched.display())),
                );
                let host = prepared.host.with_provider(&provider);
                let mut input = conflict_recovery_input(&prepared, &conflict);
                mismatch(&prepared, &mut input);

                let error = recover(&host, &prepared.run_id, input)
                    .expect_err("a mismatched checkpoint is refused");
                assert!(
                    error.to_string().contains("existing rebase matching"),
                    "{run_id}: {error}"
                );
                assert!(!launched.exists(), "{run_id}: the provider must not launch");
                assert_refused_untouched(run_id, &prepared, &host, "candidate and target\n");
            }

            let prepared = PreparedRebase::new("jrun-tampered", "README.md", "README.md");
            let conflict = stopped_conflict(&prepared);
            fs::write(
                prepared.checkout.path.join("README.md"),
                "candidate and target\n",
            )
            .unwrap();
            let provider = prepared.fixture.root.path().join("codex");
            write_executable(
                &provider,
                &provider_script(
                    "printf 'exec true\\n' >> \"$(git rev-parse --git-path rebase-merge)/git-rebase-todo\"",
                ),
            );
            let host = prepared.host.with_provider(&provider);
            let error = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect_err("rewritten recovery metadata refuses continuation");
            assert!(
                error
                    .to_string()
                    .contains("changed Git metadata identity or recovery instructions"),
                "{error}"
            );
            assert_refused_untouched("jrun-tampered", &prepared, &host, "candidate and target\n");
        },
    );
}

/// The recheck after the provider exits still governs adopted repairs: a
/// run, task or worktree that no longer owns the recovery, or a revoked
/// recovery authorization, refuses continuation.
#[cfg(unix)]
#[test]
fn earlier_repairs_are_not_continued_once_ownership_or_authorization_changed() {
    isolated_in(
        module_path!(),
        "earlier_repairs_are_not_continued_once_ownership_or_authorization_changed",
        || {
            type Change = fn(&LifecycleHost);
            let cases: [(&str, Change, &str); 2] = [
                (
                    "jrun-owner-changed",
                    |host| {
                        *host.recovery_owner_refusal.lock().unwrap() =
                            Some("the task is owned by another run".to_string());
                    },
                    "live run/task/worktree ownership changed",
                ),
                (
                    "jrun-revoked",
                    |host| {
                        *host.recovery_revocation.lock().unwrap() =
                            Some("the operator cancelled the run".to_string());
                    },
                    "recovery authorization was revoked",
                ),
            ];
            for (run_id, change, refusal) in cases {
                let prepared = PreparedRebase::new(run_id, "README.md", "README.md");
                let conflict = stopped_conflict(&prepared);
                fs::write(
                    prepared.checkout.path.join("README.md"),
                    "candidate and target\n",
                )
                .unwrap();
                let host = prepared.host.with_provider(&no_op_provider(&prepared));
                change(&host);

                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("changed ownership or authorization refuses continuation");
                assert!(error.to_string().contains(refusal), "{run_id}: {error}");
                assert_refused_untouched(run_id, &prepared, &host, "candidate and target\n");
            }
        },
    );
}
