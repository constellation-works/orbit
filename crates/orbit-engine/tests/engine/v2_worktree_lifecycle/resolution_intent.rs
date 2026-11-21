//! [F2026-11-277] Authenticated resolution intents for modify/delete conflicts.
//!
//! Keeping one side of a modify/delete conflict unchanged leaves the
//! worktree exactly as Git left the stop, so content alone never adopts it.
//! The conflict leaf states that resolution in the slot the engine bound to
//! its invocation (`resolution_intent` in its input), and the host continues
//! the stopped rebase only once the intent matches this run, step, attempt,
//! pinned base and task set, and the index stages and worktree hold exactly
//! the named side. Each test plays the dispatcher by naming the slot, and a
//! substitute provider plays the leaf by writing (or not writing) the file.
//! Every widening keeps the guard closed: missing, mismatched, foreign or
//! pre-written intents, intents for other paths or conflict kinds, links
//! and directories in the way, markers, and changed ownership all refuse.

use super::conflict_adoption::{marker_free_stop, no_op_provider, stopped_on};
use super::*;

/// The fixture task every recovery input names.
const TASK: &str = "T-REBASE";

/// The slot the dispatcher names for `prepared`'s recovery, under its real
/// scratch directory, with a fixed fresh-looking nonce.
fn slot_for(prepared: &PreparedRebase, conflict: &Value, nonce: &str) -> Value {
    let root = fs::canonicalize(&prepared.checkout.path).unwrap();
    let dir = root.join(".orbit/tmp/rebase-resolution");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}-sync_base-a1-{nonce}.json", prepared.run_id));
    json!({
        "path": path.display().to_string(),
        "schema_version": 1,
        "run_id": prepared.run_id,
        "failed_step_id": "sync_base",
        "attempt": 1,
        "nonce": nonce,
        "target_base_sha": conflict["target_base_sha"],
        "task_ids": [TASK],
    })
}

const NONCE: &str = "0123456789abcdef0123456789abcdef";

/// The intent file a leaf writes into `slot` for `resolutions`.
fn intent_for(slot: &Value, resolutions: Value) -> Value {
    let mut intent = slot.clone();
    intent.as_object_mut().unwrap().remove("path");
    intent["resolutions"] = resolutions;
    intent
}

/// A substitute provider that runs `before`, then writes `intent` to the
/// slot path, and reports success.
fn intent_provider(
    prepared: &PreparedRebase,
    slot: &Value,
    intent: &Value,
    before: &str,
) -> PathBuf {
    let provider = prepared.fixture.root.path().join("codex");
    write_executable(
        &provider,
        &provider_script(&format!(
            "{before}\ncat > '{}' <<'INTENT'\n{intent}\nINTENT",
            slot["path"].as_str().unwrap()
        )),
    );
    provider
}

/// Run conflict recovery for `prepared` with `slot` named in its input.
fn recover_with(
    prepared: &PreparedRebase,
    host: &LifecycleHost,
    conflict: &Value,
    slot: &Value,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    let mut input = conflict_recovery_input(prepared, conflict);
    input["resolution_intent"] = slot.clone();
    recover(host, &prepared.run_id, input)
}

/// The refused recovery left the stop exactly as it was: still rebasing,
/// nothing certified or widened, and the unmerged index unchanged.
fn assert_refused(case: &str, prepared: &PreparedRebase, host: &LifecycleHost, unmerged: &str) {
    let checkout = &prepared.checkout.path;
    assert!(
        rebase_in_progress(checkout),
        "{case}: the rebase stays stopped"
    );
    assert!(host.checkpoints().is_empty(), "{case}: nothing certified");
    assert!(host.widenings().is_empty(), "{case}: nothing widened");
    assert_eq!(
        git(checkout, &["ls-files", "--unmerged"]),
        unmerged,
        "{case}: every stage stays unstaged"
    );
}

/// The candidate re-lands `base.txt`, which the advanced base deleted, beside
/// an edit that keeps its pick non-empty whichever side the leaf keeps.
fn candidate_relands_deleted(run_id: &str) -> (PreparedRebase, Value) {
    let (prepared, conflict) = stopped_on(
        run_id,
        &[
            ("base.txt", Some("candidate v1\n")),
            ("lib.txt", Some("lib\n")),
        ],
        &[("base.txt", None)],
    );
    assert_eq!(conflict["conflicting_paths"], json!(["base.txt"]));
    (prepared, conflict)
}

/// [F2026-11-277] A modify/delete stop resolved by keeping a side unchanged
/// continues with the side's intent: the candidate re-landing a file its
/// base deleted, the base's edit of a file the candidate deleted, and a
/// deletion the leaf records. The worktree is exactly Git's stop in the
/// first two rows, which without the intent stays unrepaired.
#[cfg(unix)]
fn an_authenticated_keep_side_intent_continues_a_modify_delete_stop() {
    isolated_in(
        module_path!(),
        "an_authenticated_keep_side_intent_continues_a_modify_delete_stop",
        || {
            for (case, resolution) in [
                ("keep-candidate", "keep_candidate"),
                ("keep-target-base", "keep_target_base"),
                ("delete", "delete"),
            ] {
                let run_id = format!("jrun-intent-{case}");
                let (prepared, conflict, path, kept) = match case {
                    "keep-target-base" => {
                        let (prepared, conflict) = stopped_on(
                            &run_id,
                            &[("README.md", None), ("lib.txt", Some("lib\n"))],
                            &[("README.md", Some("target\n"))],
                        );
                        (prepared, conflict, "README.md", Some("target"))
                    }
                    "delete" => {
                        let (prepared, conflict) = candidate_relands_deleted(&run_id);
                        (prepared, conflict, "base.txt", None)
                    }
                    _ => {
                        let (prepared, conflict) = candidate_relands_deleted(&run_id);
                        (prepared, conflict, "base.txt", Some("candidate v1"))
                    }
                };
                assert_eq!(conflict["conflicting_paths"], json!([path]), "{case}");
                let checkout = &prepared.checkout.path;
                let slot = slot_for(&prepared, &conflict, NONCE);
                let intent = intent_for(&slot, json!([{"path": path, "resolution": resolution}]));
                let before = if kept.is_none() {
                    format!("rm -f '{path}'")
                } else {
                    String::new()
                };
                let host = prepared
                    .host
                    .with_provider(&intent_provider(&prepared, &slot, &intent, &before));
                let outcome = recover_with(&prepared, &host, &conflict, &slot)
                    .expect("an authenticated keep-side intent continues the rebase");
                assert!(outcome.success, "{case}: {:?}", outcome.message);
                assert!(!rebase_in_progress(checkout), "{case}");
                assert!(is_ancestor(checkout, &prepared.target, "HEAD"), "{case}");
                let tree = git(checkout, &["ls-tree", "--name-only", "HEAD"]);
                match kept {
                    Some(kept) => {
                        assert_eq!(git(checkout, &["show", &format!("HEAD:{path}")]), kept);
                    }
                    None => assert!(tree.lines().all(|entry| entry != path), "{case}"),
                }
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

/// Rejections 1 and 2: an untouched keep-side stop with a slot but no
/// intent stays unrepaired; an intent whose worktree differs from its named
/// side, or whose named side the index does not hold, is refused.
#[cfg(unix)]
fn missing_or_mismatched_keep_side_intents_are_refused() {
    isolated_in(
        module_path!(),
        "missing_or_mismatched_keep_side_intents_are_refused",
        || {
            for (case, resolution, before, refusal) in [
                (
                    "no-intent",
                    None,
                    "",
                    "authorized conflict files were not repaired",
                ),
                (
                    "content-differs",
                    Some("keep_candidate"),
                    "printf 'candidate v2\\n' > base.txt",
                    "keeps a side the worktree content differs from",
                ),
                (
                    "mode-differs",
                    Some("keep_candidate"),
                    "chmod +x base.txt",
                    "keeps a side whose file mode the worktree differs from",
                ),
                (
                    "missing-side",
                    Some("keep_target_base"),
                    "",
                    "names a side the index stages do not hold",
                ),
                (
                    "delete-left-file",
                    Some("delete"),
                    "",
                    "records a deletion, but the path still exists",
                ),
            ] {
                let (prepared, conflict) = candidate_relands_deleted(&format!("jrun-{case}"));
                let unmerged = git(&prepared.checkout.path, &["ls-files", "--unmerged"]);
                let slot = slot_for(&prepared, &conflict, NONCE);
                let provider = match resolution {
                    Some(resolution) => {
                        let intent = intent_for(
                            &slot,
                            json!([{"path": "base.txt", "resolution": resolution}]),
                        );
                        intent_provider(&prepared, &slot, &intent, before)
                    }
                    None => no_op_provider(&prepared),
                };
                let host = prepared.host.with_provider(&provider);
                let error = recover_with(&prepared, &host, &conflict, &slot)
                    .expect_err("a missing or mismatched intent refuses continuation");
                assert!(error.to_string().contains(refusal), "{case}: {error}");
                assert_refused(case, &prepared, &host, &unmerged);
            }
        },
    );
}

/// Rejections 3, 5 and 6: markers left in another conflict path, a marked
/// kept side, a symlink or directory in the way, a path outside the
/// authorized conflict set and a host-owned path are all refused.
#[cfg(unix)]
fn intents_never_adopt_markers_links_directories_or_foreign_paths() {
    isolated_in(
        module_path!(),
        "intents_never_adopt_markers_links_directories_or_foreign_paths",
        || {
            for (case, path, before, refusal) in [
                (
                    "marked-side",
                    "base.txt",
                    "printf '<<<<<<< HEAD\\nx\\n=======\\ny\\n>>>>>>> c\\n' > base.txt",
                    "keeps a side, but the file holds conflict markers",
                ),
                (
                    "symlink",
                    "base.txt",
                    "cp base.txt .orbit/tmp/kept.txt\nrm base.txt\nln -s .orbit/tmp/kept.txt base.txt",
                    "the path is missing, a symlink or a directory",
                ),
                (
                    "directory",
                    "base.txt",
                    "rm base.txt\nmkdir base.txt\nprintf 'candidate v1\\n' > base.txt/inner",
                    "the path is missing, a symlink or a directory",
                ),
                (
                    "outside-set",
                    "README.md",
                    "",
                    "names a path outside the authorized conflict set",
                ),
                (
                    "host-owned",
                    ".orbit/tmp/kept.txt",
                    "cp base.txt .orbit/tmp/kept.txt",
                    "names a path outside the authorized conflict set",
                ),
            ] {
                let (prepared, conflict) = candidate_relands_deleted(&format!("jrun-{case}"));
                let unmerged = git(&prepared.checkout.path, &["ls-files", "--unmerged"]);
                let slot = slot_for(&prepared, &conflict, NONCE);
                let intent = intent_for(
                    &slot,
                    json!([{"path": path, "resolution": "keep_candidate"}]),
                );
                let host = prepared
                    .host
                    .with_provider(&intent_provider(&prepared, &slot, &intent, before));
                let error = recover_with(&prepared, &host, &conflict, &slot)
                    .expect_err("the intent cannot adopt this path");
                assert!(error.to_string().contains(refusal), "{case}: {error}");
                assert_refused(case, &prepared, &host, &unmerged);
            }

            // A valid keep-side intent never vouches for another conflict
            // path still holding Git's markers.
            let (prepared, conflict) = stopped_on(
                "jrun-other-markers",
                &[
                    ("README.md", Some("candidate\n")),
                    ("base.txt", Some("candidate v1\n")),
                ],
                &[("README.md", Some("target\n")), ("base.txt", None)],
            );
            assert_eq!(
                conflict["conflicting_paths"],
                json!(["README.md", "base.txt"])
            );
            let checkout = &prepared.checkout.path;
            assert!(
                fs::read_to_string(checkout.join("README.md"))
                    .unwrap()
                    .contains("<<<<<<<")
            );
            let unmerged = git(checkout, &["ls-files", "--unmerged"]);
            let slot = slot_for(&prepared, &conflict, NONCE);
            let intent = intent_for(
                &slot,
                json!([{"path": "base.txt", "resolution": "keep_candidate"}]),
            );
            let host = prepared
                .host
                .with_provider(&intent_provider(&prepared, &slot, &intent, ""));
            let error = recover_with(&prepared, &host, &conflict, &slot)
                .expect_err("markers in another conflict path refuse continuation");
            assert!(
                error
                    .to_string()
                    .contains("authorized conflict files were not repaired"),
                "{error}"
            );
            assert_refused("other-markers", &prepared, &host, &unmerged);
        },
    );
}

/// Rejection 4: untouched binary-ancestor and custom-driver stops are
/// content conflicts, so a keep-side intent cannot adopt them either
/// [ORB-15398].
#[cfg(unix)]
fn intents_never_adopt_binary_ancestor_or_custom_driver_stops() {
    isolated_in(
        module_path!(),
        "intents_never_adopt_binary_ancestor_or_custom_driver_stops",
        || {
            for (case, ancestor, driver) in [
                ("binary-ancestor", "base\0data\n", None),
                ("custom-driver", "base\n", Some("false")),
            ] {
                let (prepared, conflict) =
                    marker_free_stop(&format!("jrun-intent-{case}"), ancestor, driver, true);
                let checkout = &prepared.checkout.path;
                let unmerged = git(checkout, &["ls-files", "--unmerged"]);
                let slot = slot_for(&prepared, &conflict, NONCE);
                let intent = intent_for(
                    &slot,
                    json!([{"path": "README.md", "resolution": "keep_target_base"}]),
                );
                let host = prepared
                    .host
                    .with_provider(&intent_provider(&prepared, &slot, &intent, ""));
                let error = recover_with(&prepared, &host, &conflict, &slot)
                    .expect_err("a content conflict is never adopted by an intent");
                assert!(
                    error
                        .to_string()
                        .contains("does not name a modify/delete conflict"),
                    "{case}: {error}"
                );
                assert_refused(case, &prepared, &host, &unmerged);
            }
        },
    );
}

/// Rejection 7: an intent not bound to this invocation (another nonce, run,
/// base, task set or attempt), a slot naming another run or path, an
/// intent written before the provider ran, and a linked intent are refused.
#[cfg(unix)]
fn intents_not_bound_to_this_invocation_are_refused() {
    isolated_in(
        module_path!(),
        "intents_not_bound_to_this_invocation_are_refused",
        || {
            type Forge = fn(&mut Value, &mut Value);
            let cases: [(&str, Forge, &str); 9] = [
                (
                    "nonce",
                    |_, intent| intent["nonce"] = json!("fedcba9876543210fedcba9876543210"),
                    "resolution intent's nonce does not name this recovery invocation",
                ),
                (
                    "run",
                    |_, intent| intent["run_id"] = json!("jrun-foreign"),
                    "resolution intent's run_id does not name this recovery invocation",
                ),
                (
                    "base",
                    |_, intent| intent["target_base_sha"] = json!("0".repeat(40)),
                    "resolution intent's target_base_sha does not name this recovery invocation",
                ),
                (
                    "task",
                    |_, intent| intent["task_ids"] = json!(["T-OTHER"]),
                    "resolution intent's task_ids does not name this recovery invocation",
                ),
                (
                    "attempt",
                    |_, intent| intent["attempt"] = json!(2),
                    "resolution intent's attempt does not name this recovery invocation",
                ),
                (
                    "unknown-field",
                    |_, intent| intent["authorized"] = json!(true),
                    "the resolution intent is malformed",
                ),
                (
                    "slot-run",
                    |slot, _| slot["run_id"] = json!("jrun-foreign"),
                    "slot does not name this run, step and pinned base",
                ),
                (
                    "slot-path",
                    |slot, _| {
                        let path = slot["path"].as_str().unwrap().replace(".json", "-x.json");
                        slot["path"] = json!(path);
                    },
                    "slot is not at its allocated path",
                ),
                (
                    "pre-written",
                    |slot, intent| {
                        fs::write(slot["path"].as_str().unwrap(), intent.to_string()).unwrap();
                    },
                    "slot was written before the provider ran",
                ),
            ];
            for (case, forge, refusal) in cases {
                let (prepared, conflict) =
                    candidate_relands_deleted(&format!("jrun-forged-{case}"));
                let unmerged = git(&prepared.checkout.path, &["ls-files", "--unmerged"]);
                let mut slot = slot_for(&prepared, &conflict, NONCE);
                let mut intent = intent_for(
                    &slot,
                    json!([{"path": "base.txt", "resolution": "keep_candidate"}]),
                );
                let provider = intent_provider(&prepared, &slot, &intent, "");
                forge(&mut slot, &mut intent);
                let provider = if matches!(case, "slot-run" | "slot-path" | "pre-written") {
                    provider
                } else {
                    intent_provider(&prepared, &slot, &intent, "")
                };
                let host = prepared.host.with_provider(&provider);
                let error = recover_with(&prepared, &host, &conflict, &slot)
                    .expect_err("an intent for another invocation is refused");
                assert!(error.to_string().contains(refusal), "{case}: {error}");
                assert_refused(case, &prepared, &host, &unmerged);
            }

            // A link at the slot path is never followed.
            let (prepared, conflict) = candidate_relands_deleted("jrun-forged-link");
            let unmerged = git(&prepared.checkout.path, &["ls-files", "--unmerged"]);
            let slot = slot_for(&prepared, &conflict, NONCE);
            let intent = intent_for(
                &slot,
                json!([{"path": "base.txt", "resolution": "keep_candidate"}]),
            );
            let elsewhere = prepared.fixture.root.path().join("intent.json");
            fs::write(&elsewhere, intent.to_string()).unwrap();
            let provider = prepared.fixture.root.path().join("codex");
            write_executable(
                &provider,
                &provider_script(&format!(
                    "ln -s '{}' '{}'",
                    elsewhere.display(),
                    slot["path"].as_str().unwrap()
                )),
            );
            let host = prepared.host.with_provider(&provider);
            let error = recover_with(&prepared, &host, &conflict, &slot)
                .expect_err("a linked intent is refused");
            assert!(
                error
                    .to_string()
                    .contains("resolution intent is a link or not a regular file"),
                "{error}"
            );
            assert_refused("link", &prepared, &host, &unmerged);
        },
    );
}

/// Rejection 8: the ownership recheck after the provider exits still
/// governs a continuation an intent would otherwise allow.
#[cfg(unix)]
fn a_valid_intent_still_refuses_changed_ownership() {
    isolated_in(
        module_path!(),
        "a_valid_intent_still_refuses_changed_ownership",
        || {
            let (prepared, conflict) = candidate_relands_deleted("jrun-intent-owner");
            let unmerged = git(&prepared.checkout.path, &["ls-files", "--unmerged"]);
            let slot = slot_for(&prepared, &conflict, NONCE);
            let intent = intent_for(
                &slot,
                json!([{"path": "base.txt", "resolution": "keep_candidate"}]),
            );
            let host = prepared
                .host
                .with_provider(&intent_provider(&prepared, &slot, &intent, ""));
            *host.recovery_owner_refusal.lock().unwrap() =
                Some("the task is owned by another run".to_string());
            let error = recover_with(&prepared, &host, &conflict, &slot)
                .expect_err("changed ownership refuses continuation");
            assert!(
                error
                    .to_string()
                    .contains("live run/task/worktree ownership changed"),
                "{error}"
            );
            assert_refused("owner", &prepared, &host, &unmerged);
        },
    );
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn resolution_intent_contracts_1() {
    isolated_in(module_path!(), "resolution_intent_contracts_1", || {
        crate::git_fixture::run_scenarios(&[
            #[cfg(unix)]
            (
                "an_authenticated_keep_side_intent_continues_a_modify_delete_stop",
                an_authenticated_keep_side_intent_continues_a_modify_delete_stop,
            ),
            #[cfg(unix)]
            (
                "missing_or_mismatched_keep_side_intents_are_refused",
                missing_or_mismatched_keep_side_intents_are_refused,
            ),
            #[cfg(unix)]
            (
                "a_valid_intent_still_refuses_changed_ownership",
                a_valid_intent_still_refuses_changed_ownership,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn resolution_intent_contracts_2() {
    isolated_in(module_path!(), "resolution_intent_contracts_2", || {
        crate::git_fixture::run_scenarios(&[
            #[cfg(unix)]
            (
                "intents_never_adopt_markers_links_directories_or_foreign_paths",
                intents_never_adopt_markers_links_directories_or_foreign_paths,
            ),
            #[cfg(unix)]
            (
                "intents_never_adopt_binary_ancestor_or_custom_driver_stops",
                intents_never_adopt_binary_ancestor_or_custom_driver_stops,
            ),
            #[cfg(unix)]
            (
                "intents_not_bound_to_this_invocation_are_refused",
                intents_not_bound_to_this_invocation_are_refused,
            ),
        ]);
    });
}
