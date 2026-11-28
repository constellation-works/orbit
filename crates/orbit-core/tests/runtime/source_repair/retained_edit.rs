//! A kept inline publication whose final recovery left its fix uncommitted
//! while a required check stayed red: the operator adopts the exact edits.

use super::failed_publication::escalated_completion_fixture;
use super::inline_publication::refused_without_effects;
use super::*;

const LINT_FIX: &str = "lint.txt";

/// The red published head, with final recovery's candidate-specific lint fix
/// left in the publisher's own checkout: one staged new file and one edit.
fn incident() -> (Fixture, PathBuf, String) {
    let (mut f, checkout, _) = escalated_completion_fixture();
    // The repair's own configured review gates its landing.
    let config = f.repo.join(".orbit/config.toml");
    let policy = std::fs::read_to_string(&config)
        .unwrap()
        .replace("before_landing = false", "before_landing = true");
    std::fs::write(config, policy).unwrap();
    super::inline_publication::reload(&mut f);
    let forge = super::escalated_publication::green_forge(&mut f);
    forge.set(&f.head, &["help-entrypoint"], &[], &[]);
    orbit_core::test_support::EngineHost::new(&f.runtime)
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
    // The published candidate changed this file, so the retained edit lands on
    // a path the squashed candidate also touched.
    let edited = "source.txt".to_string();
    std::fs::write(checkout.join(LINT_FIX), "final recovery lint fix\n").unwrap();
    git(&checkout, &["add", LINT_FIX]);
    let mut body = std::fs::read_to_string(checkout.join(&edited)).unwrap();
    body.push_str("final recovery edit\n");
    std::fs::write(checkout.join(&edited), body).unwrap();
    (f, checkout, edited)
}

fn adopting_inspection(f: &Fixture) -> Result<Value, String> {
    let mut request = f.request("inspect");
    request["source_repair"]["adopt_retained_edits"] = json!(true);
    f.runtime
        .run_tool_as_human("orbit.workflow.run.resume", request)
        .map_err(|error| error.to_string())
}

fn retained_final_recovery_edits_reach_a_fresh_same_pr_repair() {
    let (f, checkout, edited) = incident();
    let dirty = git(&checkout, &["status", "--porcelain"]);

    // Without explicit adoption the dirty checkout still refuses.
    let refused = refused_without_effects(&f, f.request("inspect"), "unadopted edits");
    assert!(refused.contains("left uncommitted work"), "{refused}");
    assert!(refused.contains("adopt_retained_edits"), "{refused}");

    let inspection = adopting_inspection(&f).unwrap();
    let bound = &inspection["binding"]["retained_patch"];
    let patch = inspection["retained_patch"].as_str().unwrap();
    assert!(patch.contains(LINT_FIX) && patch.contains("final recovery edit"));
    assert!(inspection.get("retained_patch_base64").is_none());
    assert_eq!(
        bound["sha256"],
        orbit_common::security::release::sha256_hex(patch.as_bytes())
    );
    assert_eq!(bound["base_sha"], f.head);
    assert_eq!(bound["paths"].as_array().unwrap().len(), 2);
    assert_eq!(git(&checkout, &["status", "--porcelain"]), dirty);

    let mut input = f.submit(inspection["binding"].clone());
    input["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
    input["source_repair"]["unmet_acceptance"] = json!(
        "Required help-entrypoint check failed on the published head; final recovery's lint fix was left uncommitted."
    );

    // The flag belongs to inspection; submit reads the bound patch.
    let mut flagged = input.clone();
    flagged["source_repair"]["adopt_retained_edits"] = json!(true);
    let refused = refused_without_effects(&f, flagged, "submit flag");
    assert!(refused.contains("applies to inspect"), "{refused}");

    // A patch that changed after inspection refuses before reservation.
    let lint = checkout.join(LINT_FIX);
    std::fs::write(&lint, "final recovery lint fix\nlater edit\n").unwrap();
    let refused = refused_without_effects(&f, input.clone(), "stale patch");
    assert!(refused.contains("stale"), "{refused}");
    std::fs::write(&lint, "final recovery lint fix\n").unwrap();

    // An untracked path is not an exact patch.
    let stray = checkout.join("stray.txt");
    std::fs::write(&stray, "untracked\n").unwrap();
    let refused = refused_without_effects(&f, input.clone(), "untracked path");
    assert!(refused.contains("untracked path stray.txt"), "{refused}");
    std::fs::remove_file(&stray).unwrap();

    // A moved PR head refuses before reservation.
    let original: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    let mut moved = original.clone();
    moved["headRefOid"] = json!(f.base);
    std::fs::write(&f.answer, moved.to_string()).unwrap();
    refused_without_effects(&f, input.clone(), "moved head");
    std::fs::write(&f.answer, original.to_string()).unwrap();

    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    let id = result["run_id"].as_str().unwrap().to_string();
    assert_eq!(result["completion"], "review");
    f.wait(&id);
    let run = f.runtime.show_job_run_observed(&id).unwrap();
    let admitted = run.input.as_ref().unwrap();
    assert_eq!(admitted["completion"], "review");
    assert_eq!(admitted["auto_fix"], false);
    assert_eq!(
        admitted["__source_repair"]["binding"]["retained_patch"],
        *bound
    );

    // Replay returns the original successor and spends no second attempt.
    let replayed = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    assert_eq!(replayed["run_id"], id.as_str());
    assert_eq!(replayed["replayed"], true);
    let reservations = |f: &Fixture| {
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "source_repair_reserved")
            .count()
    };
    assert_eq!(reservations(&f), 1);

    // The successor applies the exact edits, commits, validates and pushes
    // them to the same PR under the original lease.
    let repaired = super::generations::publish_applied_and_cancel(&f, &id);
    assert_ne!(repaired, f.head);
    assert_eq!(
        git(&f.repo, &["show", &format!("{repaired}:{LINT_FIX}")]),
        "final recovery lint fix"
    );
    let merged = git(&f.repo, &["show", &format!("{repaired}:{edited}")]);
    assert!(
        merged.starts_with("complete published candidate"),
        "{merged}"
    );
    assert!(merged.ends_with("final recovery edit"), "{merged}");
    let resumed =
        f.runtime.read_run_state(&id).unwrap().unwrap().pipeline["resume_candidate"].clone();
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(
        resumed["repair"]["retained_patch"]["sha256"],
        bound["sha256"]
    );
    let applied = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "source_repair_retained_patch_applied")
        .map(|entry| serde_json::from_str::<Value>(entry.note.as_deref().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0]["source_run_id"], f.source.run_id);
    assert_eq!(applied[0]["sha256"], bound["sha256"]);
    assert_eq!(applied[0]["run_id"], id.as_str());
    let log = f
        .runtime
        .get_task_artifact(&f.task, &format!("validation/{id}/0.json"))
        .unwrap()
        .unwrap();
    let log: Value = serde_json::from_slice(&log.content).unwrap();
    assert_eq!(log["tested_head"], repaired);
    assert_eq!(log["exit_code"], 0);
    let calls = std::fs::read_to_string(f._root.path().join("gh-calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("pr create "))
            .count(),
        1
    );
    assert!(!calls.lines().any(|line| line.starts_with("pr merge ")));
    assert_eq!(reservations(&f), 1);

    // The original checkout keeps its edits untouched.
    assert_eq!(git(&checkout, &["status", "--porcelain"]), dirty);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
}

/// Adoption is explicit: a clean checkout has nothing to adopt, while plain
/// inspection of it still binds no patch.
fn adoption_refuses_a_clean_checkout() {
    let (f, checkout, edited) = incident();
    git(&checkout, &["reset", "-q", "--hard", "HEAD"]);
    assert!(!checkout.join(LINT_FIX).exists());
    assert!(checkout.join(&edited).exists());
    let error = adopting_inspection(&f).unwrap_err();
    assert!(error.contains("no uncommitted edits to adopt"), "{error}");
    f.inspect();
}

/// A Latin-1 edit makes `git diff --binary` emit a literal non-UTF-8 byte; the
/// reply then carries the patch as base64, and decoding it reproduces the bound
/// size and digest exactly.
fn non_utf8_patch_is_reconstructable_from_the_inspection() {
    use base64::Engine as _;

    let (f, checkout, edited) = incident();
    let mut body = std::fs::read(checkout.join(&edited)).unwrap();
    body.extend_from_slice(b"caf\xe9\n");
    std::fs::write(checkout.join(&edited), body).unwrap();

    let inspection = adopting_inspection(&f).unwrap();
    let bound = &inspection["binding"]["retained_patch"];
    assert!(inspection.get("retained_patch").is_none(), "{inspection}");
    let encoded = inspection["retained_patch_base64"].as_str().unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert!(std::str::from_utf8(&bytes).is_err());
    assert!(bytes.windows(4).any(|w| w == b"caf\xe9"));
    assert_eq!(bound["bytes"], bytes.len());
    assert_eq!(
        bound["sha256"],
        orbit_common::security::release::sha256_hex(&bytes)
    );
}

/// A staged Git link is omitted from a patch read without entering nested
/// repositories, so adoption refuses rather than bind a path whose change the
/// bytes lack, and leaves the retained checkout as it found it.
fn adoption_refuses_a_staged_git_link() {
    let (f, checkout, _) = incident();
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    git(
        &checkout,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},nested"),
        ],
    );
    let dirty = git(&checkout, &["status", "--porcelain"]);
    assert!(dirty.contains("nested"), "{dirty}");

    let mut request = f.request("inspect");
    request["source_repair"]["adopt_retained_edits"] = json!(true);
    let error = refused_without_effects(&f, request, "staged git link");
    assert!(error.contains("nested"), "{error}");
    assert!(error.contains("Git link"), "{error}");
    assert_eq!(git(&checkout, &["status", "--porcelain"]), dirty);
}

fn adoption_refuses_an_unstaged_git_link_move() {
    let (f, checkout, _) = incident();
    let nested = checkout.join("nested");
    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    git(&nested, &["config", "user.name", "Fixture"]);
    git(&nested, &["config", "user.email", "fixture@example.com"]);
    std::fs::write(nested.join("file"), "first\n").unwrap();
    git(&nested, &["add", "file"]);
    git(&nested, &["commit", "-qm", "first"]);
    let first = git(&nested, &["rev-parse", "HEAD"]);
    let original_head = git(&checkout, &["rev-parse", "HEAD"]);
    git(
        &checkout,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{first},nested"),
        ],
    );
    git(&checkout, &["commit", "-qm", "add git link"]);
    let tree = git(&checkout, &["rev-parse", "HEAD^{tree}"]);
    let parent = git(&checkout, &["rev-parse", &format!("{original_head}^")]);
    let linked_head = git(
        &checkout,
        &["commit-tree", &tree, "-p", &parent, "-m", "fixture"],
    );
    // Keep the published identity while giving this isolated fixture a HEAD
    // tree containing the link, as a real published candidate would have.
    git(&checkout, &["replace", &original_head, &linked_head]);
    git(&checkout, &["update-ref", "HEAD", &original_head]);
    std::fs::write(nested.join("file"), "second\n").unwrap();
    git(&nested, &["commit", "-qam", "second"]);
    let nested_head = git(&nested, &["rev-parse", "HEAD"]);
    let dirty = git(&checkout, &["status", "--porcelain"]);
    assert!(dirty.contains("nested"), "{dirty}");
    assert!(
        !git(
            &checkout,
            &["-c", "diff.ignoreSubmodules=all", "status", "--porcelain"]
        )
        .contains("nested")
    );

    let mut request = f.request("inspect");
    request["source_repair"]["adopt_retained_edits"] = json!(true);
    let error = refused_without_effects(&f, request, "unstaged git link");
    assert!(
        error.contains("nested") && error.contains("Git link"),
        "{error}"
    );
    assert_eq!(git(&checkout, &["status", "--porcelain"]), dirty);
    assert_eq!(git(&nested, &["rev-parse", "HEAD"]), nested_head);
}

/// A binary retained edit conflicting with a newer integration version leaves
/// both versions recoverable in the same-PR successor (without leaving
/// unmerged index entries that would block `git_commit`), while the original
/// retained checkout stays intact.
fn retained_binary_conflict_preserves_both_sides_in_successor() {
    let (mut f, checkout, _) = escalated_completion_fixture();
    let config = f.repo.join(".orbit/config.toml");
    let policy = std::fs::read_to_string(&config)
        .unwrap()
        .replace("before_landing = false", "before_landing = true");
    std::fs::write(config, policy).unwrap();
    super::inline_publication::reload(&mut f);
    let forge = super::escalated_publication::green_forge(&mut f);
    forge.set(&f.head, &["help-entrypoint"], &[], &[]);
    orbit_core::test_support::EngineHost::new(&f.runtime)
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();

    // Advance the integration branch with a binary file while the retained
    // checkout independently adds conflicting binary bytes for the same path.
    let integration_bytes = b"\x00\x01\x02integration-binary\x00".to_vec();
    let retained_bytes = b"\x00\x01\x02retained-binary\x00".to_vec();
    git(&f.repo, &["checkout", "-q", "main"]);
    std::fs::write(f.repo.join("fixture.bin"), &integration_bytes).unwrap();
    git(&f.repo, &["add", "fixture.bin"]);
    git(&f.repo, &["commit", "-qm", "advance integration binary"]);
    git(&f.repo, &["push", "-q", "origin", "HEAD:main"]);

    std::fs::write(checkout.join("fixture.bin"), &retained_bytes).unwrap();
    git(&checkout, &["add", "fixture.bin"]);
    let dirty = git(&checkout, &["status", "--porcelain"]);

    let inspection = adopting_inspection(&f).unwrap();
    let patch_text = inspection["retained_patch"].as_str().unwrap();
    assert!(patch_text.contains("GIT binary patch"), "{inspection}");
    assert!(inspection.get("retained_patch_base64").is_none());
    assert_eq!(
        inspection["binding"]["retained_patch"]["paths"],
        json!(["fixture.bin"])
    );

    let mut input = f.submit(inspection["binding"].clone());
    input["source_repair"]["policy_digest"] = inspection["policy_digest"].clone();
    input["source_repair"]["unmet_acceptance"] =
        json!("Required help-entrypoint check failed; retained binary edit must be carried.");
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input)
        .unwrap();
    let id = result["run_id"].as_str().unwrap().to_string();
    f.wait(&id);
    f.start_actions(&id);

    let setup = f.action(
        "worktree_setup",
        json!({"run_id":id,"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let successor = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({
            "job_run_id": id,
            "task_ids": [f.task],
            "workspace_path": successor,
            "base_sha": setup["base_sha"]
        }),
    );
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["repair"]["trigger"], "conflict", "{resumed}");
    assert_eq!(
        resumed["repair"]["conflicting_paths"],
        json!(["fixture.bin"]),
        "{resumed}"
    );
    let sides = resumed["repair"]["retained_conflict_sides"]
        .as_array()
        .unwrap();
    assert_eq!(sides.len(), 1, "{resumed}");
    assert_eq!(sides[0]["path"], "fixture.bin");
    let retained_side = sides[0]["retained_path"].as_str().unwrap();
    let current_side = sides[0]["current_path"].as_str().unwrap();
    assert!(
        resumed["repair"]["output"]
            .as_str()
            .unwrap()
            .contains(retained_side),
        "{resumed}"
    );
    assert!(
        git(&successor, &["diff", "--name-only", "--diff-filter=U"]).is_empty(),
        "unmerged index entries must be cleared after saving conflict sides"
    );
    assert_eq!(
        std::fs::read(successor.join("fixture.bin")).unwrap(),
        integration_bytes
    );
    assert_eq!(
        std::fs::read(successor.join(current_side)).unwrap(),
        integration_bytes
    );
    assert_eq!(
        std::fs::read(successor.join(retained_side)).unwrap(),
        retained_bytes
    );
    assert_eq!(
        std::fs::read_to_string(successor.join("source.txt")).unwrap(),
        "complete published candidate 17\n"
    );

    // Resolve using both versions and confirm the same-PR commit/push succeeds.
    let mut resolved = std::fs::read(successor.join("fixture.bin")).unwrap();
    resolved.extend_from_slice(&std::fs::read(successor.join(retained_side)).unwrap());
    std::fs::write(successor.join("fixture.bin"), &resolved).unwrap();

    f.action(
        "git_commit",
        json!({
            "job_run_id": id,
            "scope": "all",
            "workspace_path": successor,
            "base_sha": setup["base_sha"],
            "base_ref": setup["base_ref"]
        }),
    );
    let pr_input = json!({
        "job_run_id": id,
        "completed_task_ids": [f.task],
        "workspace_path": successor,
        "head": f.branch,
        "branch": f.branch,
        "base": "main",
        "base_ref": setup["base_ref"],
        "base_sha": setup["base_sha"]
    });
    let prepared = f.action("pr_prepare", pr_input.clone());
    let mut rebase_input = pr_input.clone();
    for (key, value) in prepared.as_object().unwrap() {
        rebase_input[key] = value.clone();
    }
    f.action("git_rebase", rebase_input);
    let validated = f.action("candidate_validate", pr_input.clone());
    assert_eq!(validated["decision"], "passed");
    f.action("git_push", pr_input.clone());
    let head = git(&successor, &["rev-parse", "HEAD"]);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(head);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", pr_input);
    assert_eq!(opened["pr_number"], "17");
    assert_eq!(opened["pr_reused"], true);

    let committed_bin = crate::git_test_support::command()
        .current_dir(&f.repo)
        .args(["show", &format!("{head}:fixture.bin")])
        .output()
        .unwrap();
    assert!(committed_bin.status.success());
    assert_eq!(committed_bin.stdout, resolved);
    assert_eq!(
        git(&f.repo, &["show", &format!("{head}:source.txt")]),
        "complete published candidate 17"
    );

    // The original retained checkout stays intact.
    assert_eq!(git(&checkout, &["status", "--porcelain"]), dirty);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
    assert_eq!(
        std::fs::read(checkout.join("fixture.bin")).unwrap(),
        retained_bytes
    );
}

crate::scenario_table!(
    retained_edit_scenarios,
    retained_final_recovery_edits_reach_a_fresh_same_pr_repair,
    retained_binary_conflict_preserves_both_sides_in_successor,
    non_utf8_patch_is_reconstructable_from_the_inspection,
    adoption_refuses_a_clean_checkout,
    adoption_refuses_a_staged_git_link,
    adoption_refuses_an_unstaged_git_link_move
);
