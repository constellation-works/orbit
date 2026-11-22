//! Public task-pilot boundary for deterministic `comment-hygiene` batch
//! preparation [ORB-26787].
//!
//! Drives the real `prepare_task_pilot` and `apply_task_pilot_results`
//! deterministic actions against isolated Git repositories with tracked files,
//! carried ledger history, changes, exclusions, open-task footprints and
//! missing/malformed evidence.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskStatus};
use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{TaskArtifact, TaskType};
use serde_json::{Value, json};

use super::Workspace;

fn commit_dated(
    workspace: &Workspace,
    path: &str,
    contents: &str,
    date: &str,
    message: &str,
) -> String {
    let full = workspace.repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, contents).unwrap();
    workspace.git(&["add", "--", path]);
    let output = crate::git_test_support::command()
        .args(["commit", "-m", message])
        .current_dir(&workspace.repo)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.com")
        .env("GIT_AUTHOR_DATE", format!("{date}T12:00:00Z"))
        .env("GIT_COMMITTER_DATE", format!("{date}T12:00:00Z"))
        .output()
        .expect("spawn git commit");
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    workspace.object("HEAD")
}

fn add_comment_hygiene_task(workspace: &Workspace, title: &str, status: TaskStatus) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "Check the oldest code comments for staleness.".into(),
            acceptance_criteria: vec!["Select the verified oldest-first batch.".into()],
            status: Some(status),
            task_type: Some(TaskType::Chore),
            tags: vec!["comment-hygiene".into()],
            ..Default::default()
        })
        .unwrap()
}

fn attach_ledger(workspace: &Workspace, task_id: &str, content: &str) {
    let artifact = TaskArtifact::from_text("comment-hygiene-ledger.json", content);
    workspace
        .runtime
        .update_task_as_human(
            task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![artifact],
                ..Default::default()
            },
            "test".into(),
        )
        .unwrap();
}

fn mark_done(workspace: &Workspace, task_id: &str) {
    for status in [TaskStatus::InProgress, TaskStatus::Review, TaskStatus::Done] {
        workspace
            .runtime
            .update_task_as_human(
                task_id,
                TaskUpdateParams {
                    status: Some(status),
                    execution_summary: (status == TaskStatus::Done)
                        .then(|| "Completed fixture comment-hygiene run.".to_string()),
                    ..Default::default()
                },
                "test".into(),
            )
            .unwrap();
    }
}

fn prepare(workspace: &Workspace, task_id: &str) -> Value {
    workspace.action(
        "prepare_task_pilot",
        json!({
            "task_ids": [task_id],
            "workspace_path": workspace.repo,
            "base_branch": "main",
        }),
    )
}

fn apply_one(workspace: &Workspace, prepared: &Value, assessment: Value) -> Value {
    let task_id = prepared["task_ids"][0].as_str().unwrap();
    workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task_id],
                "tasks": [assessment],
            }],
        }),
    )
}

fn selector_assessment(task_id: &str, selectors: &[&str]) -> Value {
    json!({
        "task_id": task_id,
        "context_files_after": selectors,
        "disposition": "selectors",
        "recommended_crew": "fixture",
        "recommended_complexity": "low",
        "confidence": "high",
        "assessment_rationale": "Verified from host deterministic comment-hygiene batch.",
        "validation_approach": "Run focused tests and comment-only verification.",
        "evidence_gaps": [],
        "reassessment_triggers": [],
        "blocked_by": [],
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
        "duplicate_of": null,
        "already_landed": null,
    })
}

fn lines_content(count: usize) -> String {
    (0..count)
        .map(|i| format!("// line {i}\n"))
        .collect::<String>()
}

fn comment_hygiene_prepare_and_apply_deterministically_verify_oldest_first_batch() {
    let workspace = Workspace::new();

    // Very old excluded files: if exclusions fail, they would win oldest-first.
    commit_dated(
        &workspace,
        "vendor/third_party.rs",
        "// vendored\n",
        "2025-01-01",
        "add vendored",
    );
    commit_dated(
        &workspace,
        "tests/fixtures/sample.rs",
        "// fixture\n",
        "2025-01-02",
        "add fixture",
    );
    commit_dated(
        &workspace,
        "docs/INDEX.md",
        "# Generated index\n",
        "2025-01-03",
        "add index",
    );
    commit_dated(
        &workspace,
        "Cargo.lock",
        "# lockfile\n",
        "2025-01-04",
        "add lockfile",
    );
    commit_dated(
        &workspace,
        "src/auto_header.rs",
        "// Code generated by fixturegen. DO NOT EDIT.\nfn gen() {}\n",
        "2025-01-05",
        "add generated marker file",
    );
    commit_dated(
        &workspace,
        "assets/bundle.zip",
        "PK\x03\x04binary",
        "2025-01-06",
        "add archive",
    );
    // Excluded by open task footprints.
    commit_dated(
        &workspace,
        "src/held_by_file.rs",
        "fn held_file() {}\n",
        "2025-02-01",
        "add held_by_file",
    );
    commit_dated(
        &workspace,
        "owned_dir/held_by_dir.rs",
        "fn held_dir() {}\n",
        "2025-02-02",
        "add held_by_dir",
    );
    commit_dated(
        &workspace,
        "src/held_by_symbol.rs",
        "fn run() {}\n",
        "2025-02-03",
        "add held_by_symbol",
    );

    // Files tracked in carried ledger history.
    commit_dated(
        &workspace,
        "src/changed_old.rs",
        "fn old_v1() {}\n",
        "2026-01-10",
        "add changed_old",
    );
    commit_dated(
        &workspace,
        "src/changed_newer.rs",
        "fn newer_v1() {}\n",
        "2026-01-11",
        "add changed_newer",
    );
    commit_dated(
        &workspace,
        "src/repeat_skip.rs",
        "fn skip_me() {}\n",
        "2026-01-12",
        "add repeat_skip",
    );
    let checked_base = commit_dated(
        &workspace,
        "src/unchanged.rs",
        "fn stable() {}\n",
        "2026-01-13",
        "add unchanged",
    );

    // Modify the two changed files AFTER checked_base; leave unchanged.rs untouched.
    commit_dated(
        &workspace,
        "src/changed_old.rs",
        "fn old_v2() {}\n",
        "2026-03-01",
        "edit changed_old",
    );
    commit_dated(
        &workspace,
        "src/changed_newer.rs",
        "fn newer_v2() {}\n",
        "2026-03-02",
        "edit changed_newer",
    );

    // Never-checked files with explicit git dates and a tie on 2026-02-01.
    // README.md from Workspace::new() has today's date, so these older files rank first.
    commit_dated(
        &workspace,
        "src/never_b.rs",
        "fn b() {}\n",
        "2026-02-01",
        "add never_b",
    );
    commit_dated(
        &workspace,
        "src/never_a.rs",
        "fn a() {}\n",
        "2026-02-01",
        "add never_a",
    );
    commit_dated(
        &workspace,
        "src/never_c.rs",
        "fn c() {}\n",
        "2026-02-05",
        "add never_c",
    );
    commit_dated(
        &workspace,
        "src/never_d.rs",
        "fn d() {}\n",
        "2026-02-10",
        "add never_d",
    );
    commit_dated(
        &workspace,
        "src/never_e.rs",
        "fn e() {}\n",
        "2026-02-15",
        "add never_e",
    );

    // Older done predecessor carrying a schema_version 2 cumulative ledger.
    let pred_v2 = add_comment_hygiene_task(&workspace, "comment-hygiene v2", TaskStatus::Backlog);
    let v2_ledger = json!({
        "schema_version": 2,
        "run_date": "2026-10-01",
        "head": checked_base,
        "files": [],
        "state": {
            "src/changed_old.rs": [{
                "attempted_on": "2026-10-01",
                "outcome": "clean",
                "checked_through": checked_base,
            }],
            "src/unchanged.rs": [{
                "attempted_on": "2026-09-01",
                "outcome": "clean",
                "checked_through": checked_base,
            }],
            "src/repeat_skip.rs": [
                {
                    "attempted_on": "2026-10-02",
                    "outcome": "skipped",
                    "reason": "still too large",
                    "checked_through": checked_base,
                },
                {
                    "attempted_on": "2026-09-25",
                    "outcome": "partial",
                    "reason": "only first half checked",
                    "checked_through": checked_base,
                }
            ],
            "src/deleted.rs": [{
                "attempted_on": "2026-09-01",
                "outcome": "clean",
                "checked_through": checked_base,
            }]
        }
    });
    attach_ledger(
        &workspace,
        &pred_v2.id,
        &serde_json::to_string(&v2_ledger).unwrap(),
    );
    mark_done(&workspace, &pred_v2.id);

    // Newer done predecessor carrying a schema_version 1 ledger layered over v2.
    let pred_v1 = add_comment_hygiene_task(&workspace, "comment-hygiene v1", TaskStatus::Backlog);
    let v1_ledger = json!({
        "schema_version": 1,
        "run_date": "2026-10-05",
        "head": checked_base,
        "files": [{
            "path": "src/changed_newer.rs",
            "attempted_on": "2026-10-05",
            "outcome": "fixed",
            "checked_through": checked_base,
        }]
    });
    attach_ledger(
        &workspace,
        &pred_v1.id,
        &serde_json::to_string(&v1_ledger).unwrap(),
    );
    mark_done(&workspace, &pred_v1.id);

    // Completed escalation task (no ledger) must not trigger a missing-ledger hold.
    let escalation = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Check comments in src/repeat_skip.rs range by range".into(),
            description: "Dedicated escalation task.".into(),
            acceptance_criteria: vec!["Check ranges.".into()],
            status: Some(TaskStatus::Backlog),
            task_type: Some(TaskType::Chore),
            tags: vec![
                "comment-hygiene".into(),
                "comment-hygiene-escalation".into(),
            ],
            context_files: vec!["file:src/repeat_skip.rs".into()],
            ..Default::default()
        })
        .unwrap();
    mark_done(&workspace, &escalation.id);

    // Open tasks holding footprints that must be excluded.
    let open_file = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Open file holder".into(),
            description: "Owns src/held_by_file.rs".into(),
            acceptance_criteria: vec!["Done.".into()],
            status: Some(TaskStatus::Backlog),
            context_files: vec!["file:src/held_by_file.rs".into()],
            ..Default::default()
        })
        .unwrap();
    let open_dir = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Open dir holder".into(),
            description: "Owns owned_dir".into(),
            acceptance_criteria: vec!["Done.".into()],
            status: Some(TaskStatus::InProgress),
            context_files: vec!["dir:owned_dir".into()],
            ..Default::default()
        })
        .unwrap();
    let open_symbol = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Open symbol holder".into(),
            description: "Owns src/held_by_symbol.rs#run:function".into(),
            acceptance_criteria: vec!["Done.".into()],
            status: Some(TaskStatus::Review),
            context_files: vec!["symbol:src/held_by_symbol.rs#run:function".into()],
            ..Default::default()
        })
        .unwrap();

    // The selector-free proposed comment-hygiene task under preparation.
    let current = add_comment_hygiene_task(
        &workspace,
        "Check the oldest code comments for staleness",
        TaskStatus::Proposed,
    );

    let prepared = prepare(&workspace, &current.id);
    let batch = &prepared["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch["status"], "ready", "{prepared}");
    assert_eq!(
        prepared["partitions"][0]["comment_hygiene_batches"][&current.id], *batch,
        "partition must carry the exact deterministic batch for the pilot: {prepared}"
    );

    let expected_selectors = [
        "file:src/changed_old.rs",
        "file:src/changed_newer.rs",
        "file:src/never_a.rs",
        "file:src/never_b.rs",
        "file:src/never_c.rs",
        "file:src/never_d.rs",
    ];
    assert_eq!(batch["selectors"], json!(expected_selectors), "{batch}");
    let ranked: Vec<&str> = batch["ranked_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["selector"].as_str().unwrap())
        .collect();
    assert!(
        ranked.starts_with(&expected_selectors),
        "ranked_candidates must start with selected batch: {ranked:?}"
    );
    assert!(
        ranked.contains(&"file:src/never_e.rs"),
        "ranked_candidates must retain deterministic overflow candidates: {ranked:?}"
    );

    // Verify ledger provenance and reconciliation counts.
    assert_eq!(batch["ledger"]["status"], "loaded", "{batch}");
    assert_eq!(batch["ledger"]["base_v2_task_id"], pred_v2.id, "{batch}");
    assert_eq!(
        batch["ledger"]["v1_layered_task_ids"],
        json!([pred_v1.id]),
        "{batch}"
    );
    assert_eq!(batch["ledger"]["dropped_untracked_paths"], 1, "{batch}");
    assert_eq!(batch["inventory"]["unchanged_dropped"], 1, "{batch}");
    assert_eq!(batch["inventory"]["repeat_skip_held_count"], 1, "{batch}");
    assert_eq!(
        batch["inventory"]["repeat_skip_held"][0]["path"], "src/repeat_skip.rs",
        "{batch}"
    );
    assert_eq!(batch["inventory"]["open_task_excluded_count"], 3, "{batch}");
    let open_excluded = batch["inventory"]["open_task_excluded"].as_array().unwrap();
    for (path, holder) in [
        ("src/held_by_file.rs", open_file.id.as_str()),
        ("owned_dir/held_by_dir.rs", open_dir.id.as_str()),
        ("src/held_by_symbol.rs", open_symbol.id.as_str()),
    ] {
        assert!(
            open_excluded
                .iter()
                .any(|row| row["path"] == path && row["task_id"] == holder),
            "missing open-task exclusion ({path}, {holder}) in {open_excluded:?}"
        );
    }

    // Apply refuses an agent guessing an excluded/out-of-order selector and
    // preserves the deterministic batch on the repair partition.
    let bad = apply_one(
        &workspace,
        &prepared,
        selector_assessment(&current.id, &["file:src/held_by_file.rs"]),
    );
    assert_eq!(bad["applied_count"], 0, "{bad}");
    assert_eq!(bad["repair_count"], 1, "{bad}");
    assert_eq!(
        bad["repair_partitions"][0]["comment_hygiene_batches"][&current.id], *batch,
        "repair partition must retain the deterministic comment-hygiene batch: {bad}"
    );
    assert!(
        workspace
            .runtime
            .get_task(&current.id)
            .unwrap()
            .context_files
            .is_empty(),
        "refused provisional/out-of-contract batch must not persist selectors"
    );

    // Apply accepts the verified oldest-first batch and locks the exact selectors.
    let applied = apply_one(
        &workspace,
        &prepared,
        selector_assessment(&current.id, &expected_selectors),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(
        workspace
            .runtime
            .get_task(&current.id)
            .unwrap()
            .context_files,
        expected_selectors
    );
}

fn comment_hygiene_enforces_line_budget_and_oversized_file_solo_rule() {
    // Subcase 1: An >800-line file first in queue is taken alone with ranged=true.
    let workspace = Workspace::new();
    commit_dated(
        &workspace,
        "src/huge.rs",
        &lines_content(850),
        "2026-01-01",
        "add huge",
    );
    commit_dated(
        &workspace,
        "src/small.rs",
        &lines_content(20),
        "2026-01-02",
        "add small",
    );
    let task = add_comment_hygiene_task(&workspace, "oversized first", TaskStatus::Proposed);
    let prepared = prepare(&workspace, &task.id);
    let batch = &prepared["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch["status"], "ready", "{batch}");
    assert_eq!(batch["selectors"], json!(["file:src/huge.rs"]), "{batch}");
    assert_eq!(batch["selected"][0]["ranged"], true, "{batch}");
    assert_eq!(batch["selected"][0]["lines"], 850, "{batch}");

    // Subcase 2: Line budget stops before exceeding 1500 lines.
    let workspace2 = Workspace::new();
    commit_dated(
        &workspace2,
        "src/a.rs",
        &lines_content(700),
        "2026-01-01",
        "add a",
    );
    commit_dated(
        &workspace2,
        "src/b.rs",
        &lines_content(700),
        "2026-01-02",
        "add b",
    );
    commit_dated(
        &workspace2,
        "src/c.rs",
        &lines_content(300),
        "2026-01-03",
        "add c",
    );
    let task2 = add_comment_hygiene_task(&workspace2, "line budget", TaskStatus::Proposed);
    let prepared2 = prepare(&workspace2, &task2.id);
    let batch2 = &prepared2["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch2["status"], "ready", "{batch2}");
    assert_eq!(
        batch2["selectors"],
        json!(["file:src/a.rs", "file:src/b.rs"]),
        "{batch2}"
    );
    assert_eq!(batch2["total_selected_lines"], 1400, "{batch2}");

    // Subcase 3: An >800-line file second in queue does not join a multi-file batch.
    let workspace3 = Workspace::new();
    commit_dated(
        &workspace3,
        "src/first.rs",
        &lines_content(500),
        "2026-01-01",
        "add first",
    );
    commit_dated(
        &workspace3,
        "src/second_huge.rs",
        &lines_content(850),
        "2026-01-02",
        "add second_huge",
    );
    let task3 = add_comment_hygiene_task(&workspace3, "oversized second", TaskStatus::Proposed);
    let prepared3 = prepare(&workspace3, &task3.id);
    let batch3 = &prepared3["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch3["status"], "ready", "{batch3}");
    assert_eq!(
        batch3["selectors"],
        json!(["file:src/first.rs"]),
        "{batch3}"
    );
}

fn comment_hygiene_missing_ledger_or_footprint_produces_honest_hold_without_provisional_selectors()
{
    // Subcase 1: Missing predecessor ledger -> honest hold, never publishes provisional selectors.
    let workspace = Workspace::new();
    commit_dated(&workspace, "src/a.rs", "fn a() {}\n", "2026-01-01", "add a");
    let missing_ledger_pred =
        add_comment_hygiene_task(&workspace, "pred without ledger", TaskStatus::Backlog);
    mark_done(&workspace, &missing_ledger_pred.id);

    let current = add_comment_hygiene_task(&workspace, "current run", TaskStatus::Proposed);
    let prepared = prepare(&workspace, &current.id);
    let batch = &prepared["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch["status"], "hold", "{batch}");
    assert_eq!(batch["selectors"], json!([]), "{batch}");
    assert_eq!(batch["ranked_candidates"], json!([]), "{batch}");
    let reasons = batch["hold_reasons"].as_array().unwrap();
    assert!(
        reasons.iter().any(|r| {
            r["code"] == "missing_predecessor_ledger"
                && r["source"]
                    .as_str()
                    .is_some_and(|s| s.contains(&missing_ledger_pred.id))
        }),
        "expected missing_predecessor_ledger naming {}: {reasons:?}",
        missing_ledger_pred.id
    );

    // Apply MUST refuse provisional selectors while held.
    let provisional = apply_one(
        &workspace,
        &prepared,
        selector_assessment(&current.id, &["file:src/a.rs"]),
    );
    assert_eq!(provisional["applied_count"], 0, "{provisional}");
    assert_eq!(provisional["repair_count"], 1, "{provisional}");
    assert!(
        workspace
            .runtime
            .get_task(&current.id)
            .unwrap()
            .context_files
            .is_empty()
    );

    // Apply MUST also refuse verified_no_diff while held.
    let fake_no_diff = apply_one(
        &workspace,
        &prepared,
        json!({
            "task_id": current.id,
            "context_files_after": [],
            "disposition": "verified_no_diff",
            "evidence": "Pretending no diff despite missing ledger.",
            "recommended_crew": "fixture",
            "recommended_complexity": "low",
            "confidence": "high",
            "assessment_rationale": "Cannot be verified no diff.",
            "validation_approach": "None.",
            "evidence_gaps": [],
            "reassessment_triggers": [],
            "blocked_by": [],
            "adr_conflicts": [],
            "utility_warnings": [],
            "surface_warnings": [],
            "duplicate_of": null,
            "already_landed": null,
        }),
    );
    assert_eq!(fake_no_diff["applied_count"], 0, "{fake_no_diff}");
    assert_eq!(fake_no_diff["repair_count"], 1, "{fake_no_diff}");

    // Apply ADMITS an honest hold (empty selectors, disposition "selectors") and stamps FOOTPRINT_UNRESOLVED.
    let held = apply_one(
        &workspace,
        &prepared,
        json!({
            "task_id": current.id,
            "context_files_after": [],
            "disposition": "selectors",
            "recommended_crew": "fixture",
            "recommended_complexity": "low",
            "confidence": "low",
            "assessment_rationale": "Predecessor ledger is missing; holding without provisional selectors.",
            "validation_approach": "Attach the missing predecessor ledger and repilot.",
            "evidence_gaps": [format!("{}:comment-hygiene-ledger.json is missing", missing_ledger_pred.id)],
            "reassessment_triggers": ["Attach missing comment-hygiene-ledger.json and repilot."],
            "blocked_by": [],
            "adr_conflicts": [],
            "utility_warnings": [{
                "text": format!("Footprint unresolved: unread predecessor ledger {}:comment-hygiene-ledger.json.", missing_ledger_pred.id),
                "holds_promotion": true,
            }],
            "surface_warnings": [],
            "duplicate_of": null,
            "already_landed": null,
        }),
    );
    assert_eq!(held["status"], "succeeded", "{held}");
    assert_eq!(held["applied_count"], 1, "{held}");
    let stored = workspace.runtime.get_task(&current.id).unwrap();
    assert_eq!(stored.status, TaskStatus::Proposed);
    assert!(
        stored.context_files.is_empty(),
        "{:?}",
        stored.context_files
    );
    let comments = workspace.runtime.get_task_comments(&current.id).unwrap();
    let warnings = workspace
        .runtime
        .task_pilot_warning_findings(&stored, &comments, &|_| false);
    assert!(
        !warnings.is_empty(),
        "held comment-hygiene task must retain a promotion-holding warning"
    );

    // Subcase 2: Malformed predecessor ledger -> honest hold.
    let workspace2 = Workspace::new();
    commit_dated(
        &workspace2,
        "src/a.rs",
        "fn a() {}\n",
        "2026-01-01",
        "add a",
    );
    let corrupt_pred =
        add_comment_hygiene_task(&workspace2, "pred corrupt ledger", TaskStatus::Backlog);
    attach_ledger(&workspace2, &corrupt_pred.id, "{not valid json");
    mark_done(&workspace2, &corrupt_pred.id);
    let current2 = add_comment_hygiene_task(&workspace2, "current 2", TaskStatus::Proposed);
    let prepared2 = prepare(&workspace2, &current2.id);
    let batch2 = &prepared2["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch2["status"], "hold", "{batch2}");
    assert_eq!(batch2["selectors"], json!([]), "{batch2}");
    assert!(
        batch2["hold_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["code"] == "ledger_malformed"),
        "{batch2}"
    );

    // Subcase 3: Corrupt/unparseable open-task footprint selector -> honest hold.
    let workspace3 = Workspace::new();
    commit_dated(
        &workspace3,
        "src/a.rs",
        "fn a() {}\n",
        "2026-01-01",
        "add a",
    );
    // Insert an open task carrying a malformed selector directly into the store
    // (simulating a pre-validation or corrupt footprint record).
    let bad_open: TaskCreateParams = serde_json::from_value(json!({
        "actor": "test",
        "title": "Open task with malformed selector",
        "description": "Corrupt footprint.",
        "acceptance_criteria": ["Fix."],
        "dependencies": [],
        "relations": [],
        "tags": [],
        "required_tools": [],
        "plan": "",
        "execution_summary": "",
        "context_files": ["symbol:malformed-without-hash-or-kind"],
        "created_by": "test",
        "status": TaskStatus::Backlog,
        "priority": "medium",
        "task_type": "chore",
        "external_refs": [],
        "comments": [],
    }))
    .unwrap();
    let bad_open_task = workspace3
        .runtime
        .stores()
        .tasks()
        .create_task(bad_open)
        .unwrap();
    let current3 = add_comment_hygiene_task(&workspace3, "current 3", TaskStatus::Proposed);
    let prepared3 = prepare(&workspace3, &current3.id);
    let batch3 = &prepared3["tasks"][0]["comment_hygiene_batch"];
    assert_eq!(batch3["status"], "hold", "{batch3}");
    assert_eq!(batch3["selectors"], json!([]), "{batch3}");
    assert_eq!(batch3["ranked_candidates"], json!([]), "{batch3}");
    assert!(
        batch3["hold_reasons"].as_array().unwrap().iter().any(|r| {
            r["code"] == "open_task_selector_unparseable"
                && r["source"]
                    .as_str()
                    .is_some_and(|s| s.contains(&bad_open_task.id))
        }),
        "expected open_task_selector_unparseable naming {}: {batch3}",
        bad_open_task.id
    );
}

crate::scenario_table!(
    comment_hygiene_pilot_batch_preparation_scenarios,
    comment_hygiene_prepare_and_apply_deterministically_verify_oldest_first_batch,
    comment_hygiene_enforces_line_budget_and_oversized_file_solo_rule,
    comment_hygiene_missing_ledger_or_footprint_produces_honest_hold_without_provisional_selectors
);
