//! Review evidence leaves the footprint only with an explicit inspection
//! reason. Unexplained omissions survive reassessment, and over-attachment
//! findings still fire for swept proposals. Fixtures drive the real prepare
//! and apply actions against a real store and repository in isolated children.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams, TaskWriteScreen};
use orbit_core::{Task, TaskStatus};
use serde_json::{Value, json};

use super::Workspace;

/// The shape of a filed finding: the source paths its evidence cites, in
/// the order the review filed them, plus the regression-test location.
const DUPLICATES: &str = "file:src/dependabot/duplicates.rs";
const FILING: &str = "file:src/dependabot/filing.rs";
const ADMISSION: &str = "file:src/admission/duplicate_tasks.rs";
const CODE_GROUPS: &str = "file:src/dependabot/code_groups.rs";
const SWEEP_TEST: &str = "file:tests/runtime/security_alert_sweep.rs";
const FILED: [&str; 5] = [DUPLICATES, FILING, ADMISSION, CODE_GROUPS, SWEEP_TEST];
/// What a pilot proposing only the edit targets keeps.
const NARROWED: [&str; 2] = [ADMISSION, SWEEP_TEST];
const EXTRA: &str = "file:src/dependabot/mod.rs";

impl Workspace {
    fn commit_files(&self, selectors: &[&str]) {
        for selector in selectors {
            let path = self.repo.join(selector.trim_start_matches("file:"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fn fixture() {}\n").unwrap();
        }
        self.git(&["add", "."]);
        self.git(&["commit", "-m", "fixture sources"]);
    }

    fn filed_task(&self, title: &str, tags: &[&str], context_files: &[&str]) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Repair {title}."),
                acceptance_criteria: vec!["The duplicate is suppressed once.".into()],
                plan: "Inspect the cited sources.".into(),
                status: Some(TaskStatus::Proposed),
                tags: owned(tags),
                context_files: owned(context_files),
                ..Default::default()
            })
            .unwrap()
    }

    fn assess(&self, prepared: &Value, after: &[&str], complexity: &str) -> Value {
        self.assess_with_drops(prepared, after, complexity, json!([]))
    }

    fn assess_with_drops(
        &self,
        prepared: &Value,
        after: &[&str],
        complexity: &str,
        drops: Value,
    ) -> Value {
        let task_id = prepared["task_ids"][0].clone();
        self.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": self.repo,
                "prepared": prepared,
                "results": [{
                    "partition_index": 0, "task_ids": [task_id],
                    "tasks": [{
                        "task_id": task_id, "context_files_after": after,
                        "context_evidence_dropped": drops,
                        "disposition": "selectors", "recommended_crew": "fixture",
                        "recommended_complexity": complexity, "confidence": "high",
                        "assessment_rationale": "The cited sources contain the defect.",
                        "validation_approach": "Inspect the persisted scope.",
                        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                        "duplicate_of": null, "already_landed": null,
                    }],
                }],
            }),
        )
    }

    fn scope(&self, task_id: &str) -> Vec<String> {
        self.runtime.get_task(task_id).unwrap().context_files
    }
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn outcome(applied: &Value) -> &Value {
    &applied["task_outcomes"][0]
}

fn root_selectors_apply_at_the_pinned_source() {
    for after in [&["dir:."][..], &["dir:.", "file:README.md"][..]] {
        let workspace = Workspace::new();
        let task = workspace.filed_task("root scope", &[], &["file:README.md"]);
        let pinned = workspace.object("HEAD");
        let prepared = workspace.prepare(&[&task.id]);
        assert_eq!(prepared["source"]["source_revision"], pinned);

        // The assessment remains bound to preparation even when the live
        // checkout and branch no longer contain its narrower target.
        workspace.git(&["rm", "README.md"]);
        workspace.git(&["commit", "-m", "remove the live target"]);
        assert_ne!(workspace.object("HEAD"), pinned);
        assert!(!workspace.repo.join("README.md").exists());

        let applied = workspace.assess(&prepared, after, "medium");
        assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert_eq!(workspace.scope(&task.id), after);
    }
}

fn omitted_root_is_retained_as_review_evidence_and_is_never_missing() {
    for tags in [&["code-review"][..], &[][..]] {
        let workspace = Workspace::new();
        let filed = ["dir:.", "file:README.md"];
        let task = workspace.filed_task("root evidence", tags, &filed);
        let prepared = workspace.prepare(&[&task.id]);
        assert_eq!(
            prepared["source"]["source_revision"],
            workspace.object("HEAD")
        );
        let applied = workspace.assess(&prepared, &["file:README.md"], "medium");
        assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert!(
            applied["tasks"][0]
                .get("context_reauthorization_required")
                .is_none(),
            "the omitted root exists at the pinned source: {applied}"
        );
        if tags.is_empty() {
            // Non-review narrowing reaches missing-target inspection with
            // the root still omitted, rather than hidden by retention.
            assert_eq!(workspace.scope(&task.id), ["file:README.md"]);
        } else {
            assert_eq!(
                applied["tasks"][0]["context_evidence_retained"],
                json!(["dir:."])
            );
            assert_eq!(workspace.scope(&task.id), filed);
        }
    }
}

fn root_assessments_preserve_pinned_path_confinement_and_kind_checks() {
    let workspace = Workspace::new();
    workspace.commit_file("src/module.rs", "fn fixture() {}\n", "add a directory");
    let commit = workspace.object("HEAD");
    workspace.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000",
        &commit,
        "submodule",
    ]);
    workspace.git(&["commit", "-m", "add an unsupported gitlink"]);
    let mut refused = vec![
        "dir:..".to_string(),
        "dir:../repo".to_string(),
        format!("dir:{}", workspace.root.path().display()),
        format!("dir:{}", workspace.repo.display()),
        "module:fixture".to_string(),
        "command:orbit".to_string(),
        "file:.".to_string(),
        "symbol:.#root:function".to_string(),
        "file:src".to_string(),
        "dir:README.md".to_string(),
        "dir:submodule".to_string(),
        "file:later.rs".to_string(),
    ];
    #[cfg(unix)]
    {
        // Both an in-repository link and an escaping link remain unsupported
        // Git objects even when the assessment also contains the root.
        std::os::unix::fs::symlink("README.md", workspace.repo.join("link.rs")).unwrap();
        std::os::unix::fs::symlink("..", workspace.repo.join("link-dir")).unwrap();
        workspace.git(&["add", "link.rs", "link-dir"]);
        workspace.git(&["commit", "-m", "add symlinks"]);
        refused.extend(["file:link.rs".into(), "dir:link-dir".into()]);
    }
    let task = workspace.filed_task("confined root", &[], &["file:README.md"]);
    let prepared = workspace.prepare(&[&task.id]);
    workspace.commit_file("later.rs", "fn later() {}\n", "advance after preparation");
    for selector in refused {
        let applied = workspace.assess(&prepared, &["dir:.", &selector], "medium");
        assert_eq!(
            outcome(&applied)["outcome"],
            "invalid",
            "{selector}: {applied}"
        );
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    }
    let applied = workspace.assess(
        &prepared,
        &["dir:.", "dir:src", "file:src/module.rs"],
        "medium",
    );
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        workspace.scope(&task.id),
        ["dir:.", "dir:src", "file:src/module.rs"]
    );
}

fn review_evidence_survives_narrowing_reassessment_and_replay() {
    let workspace = Workspace::new();
    workspace.commit_files(&[&FILED[..], &[EXTRA]].concat());
    let task = workspace.filed_task("duplicate alert", &["delivery-code-review"], &FILED);

    // The pilot proposes only what it would edit; apply keeps the evidence
    // it omitted, in the order the review filed it.
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        applied["tasks"][0]["context_evidence_retained"],
        json!([DUPLICATES, FILING, CODE_GROUPS]),
        "{applied}"
    );
    assert_eq!(applied["tasks"][0]["context_files_after"], json!(FILED));
    assert_eq!(workspace.scope(&task.id), FILED);

    // Replaying the same apply settles as already applied.
    let replayed = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(
        outcome(&replayed)["outcome"],
        "already_applied",
        "{replayed}"
    );

    // Each re-assessment that narrows again rewrites nothing.
    for _ in 0..2 {
        let prepared = workspace.prepare(&[&task.id]);
        let reassessed = workspace.assess(&prepared, &NARROWED, "medium");
        assert_eq!(outcome(&reassessed)["outcome"], "applied", "{reassessed}");
        assert_eq!(reassessed["tasks"][0]["applied"], false, "{reassessed}");
        assert_eq!(workspace.scope(&task.id), FILED);
    }

    // The pilot can still widen it with a target the evidence did not name.
    let prepared = workspace.prepare(&[&task.id]);
    let widened = workspace.assess(&prepared, &[ADMISSION, EXTRA], "medium");
    assert_eq!(outcome(&widened)["outcome"], "applied", "{widened}");
    assert_eq!(
        workspace.scope(&task.id),
        owned(&[&FILED[..], &[EXTRA]].concat())
    );

    // An operator can narrow it without a pilot drop record.
    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                context_files: Some(owned(&NARROWED)),
                ..Default::default()
            },
            Some("codex".into()),
            Some("fixture-model".into()),
        )
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let respected = workspace.assess(&prepared, &[ADMISSION], "medium");
    assert_eq!(outcome(&respected)["outcome"], "applied", "{respected}");
    assert_eq!(workspace.scope(&task.id), NARROWED);
}

fn verified_read_only_evidence_can_leave_the_lock_while_change_targets_remain() {
    let workspace = Workspace::new();
    workspace.commit_files(&FILED);
    let task = workspace.filed_task("duplicate alert", &["delivery-code-review"], &FILED);
    let task = workspace.runtime.update_task_with_identity(
        &task.id,
        TaskUpdateParams {
            description: Some(format!(
                "Change {ADMISSION} and {SWEEP_TEST}. Read-only evidence: {DUPLICATES}, {FILING}, {CODE_GROUPS}."
            )),
            ..Default::default()
        },
        Some("codex".into()),
        Some("fixture-model".into()),
    ).unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let drops = json!([
        {"selector": DUPLICATES, "reason": "Verified the duplicate caller is read-only evidence; repair is in admission."},
        {"selector": FILING, "reason": "Verified filing only supplies evidence and requires no change."},
        {"selector": CODE_GROUPS, "reason": "Verified grouping is only consulted; the finding targets admission and its regression test."},
    ]);
    let applied = workspace.assess_with_drops(&prepared, &NARROWED, "medium", drops.clone());
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        workspace.scope(&task.id),
        NARROWED,
        "verified read-only evidence must not reserve locks"
    );
    assert!(
        applied["tasks"][0]
            .get("context_evidence_retained")
            .is_none()
    );
    assert_eq!(applied["tasks"][0]["context_evidence_dropped"], drops);
    let current = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(
        current.description, task.description,
        "evidence remains in the finding description"
    );
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit: Value = super::pilot_audit(&workspace.runtime, &task.id, comments.last().unwrap());
    assert_eq!(
        audit["assessment"]["context_evidence_dropped"], drops,
        "drop reasons must be durable"
    );
    let replayed = workspace.assess_with_drops(&prepared, &NARROWED, "medium", drops);
    assert_eq!(
        outcome(&replayed)["outcome"],
        "already_applied",
        "{replayed}"
    );
}

fn invalid_evidence_drop_records_cannot_narrow_the_scope() {
    let workspace = Workspace::new();
    workspace.commit_files(&[&FILED[..], &[EXTRA]].concat());
    let task = workspace.filed_task("duplicate alert", &["code-review"], &FILED);
    let prepared = workspace.prepare(&[&task.id]);
    for drops in [
        json!({"selector": DUPLICATES, "reason": "read only"}),
        json!([{"selector": DUPLICATES}]),
        json!([{"selector": DUPLICATES, "reason": "  "}]),
        json!([{"selector": EXTRA, "reason": "not in the finding"}]),
        json!([{"selector": ADMISSION, "reason": "conflicts with an explicit change target"}]),
        json!([{"selector": DUPLICATES, "reason": "read only"}, {"selector": DUPLICATES, "reason": "duplicate"}]),
    ] {
        let applied = workspace.assess_with_drops(&prepared, &NARROWED, "medium", drops);
        assert_eq!(outcome(&applied)["outcome"], "invalid", "{applied}");
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    }
}

fn no_diff_input(workspace: &Workspace, prepared: &Value, drops: Value) -> Value {
    let task_id = &prepared["task_ids"][0];
    let evidence = format!(
        "The finding is already satisfied by {} at the pinned source.",
        prepared["source"]["source_revision"].as_str().unwrap()
    );
    json!({
        "workspace_path": workspace.repo, "prepared": prepared,
        "results": [{"partition_index": 0, "task_ids": [task_id], "tasks": [{
            "task_id": task_id, "context_files_after": [],
            "disposition": "verified_no_diff", "evidence": evidence,
            "already_landed": {"evidence": evidence},
            "context_evidence_dropped": drops,
            "recommended_crew": "fixture", "recommended_complexity": "medium",
            "confidence": "high", "assessment_rationale": "The pinned sources need no change.",
            "validation_approach": "Verify source proof, scope, audit and replay.",
            "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
            "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
            "duplicate_of": null,
        }]}],
    })
}

fn verified_no_diff_accepts_evidence_drops_on_initial_and_targeted_repair_apply() {
    for repair in [false, true] {
        let workspace = Workspace::new();
        workspace.commit_files(&FILED);
        let task = workspace.filed_task(
            "already satisfied review",
            &["delivery-code-review"],
            &FILED,
        );
        let pinned = workspace.object("HEAD");
        let prepared = workspace.prepare(&[&task.id]);
        assert_eq!(prepared["source"]["source_revision"], pinned);
        let drops = json!([
            {"selector": DUPLICATES, "reason": "Inspected the pinned caller; the duplicate is already suppressed."},
            {"selector": FILING, "reason": "Inspected filing at the pinned source; it needs no change."},
        ]);
        let input = if repair {
            // Exercise the real targeted-repair payload, retaining preparation
            // and carried outcomes after a malformed first assessment.
            let first = workspace.action(
                "apply_task_pilot_results",
                no_diff_input(
                    &workspace,
                    &prepared,
                    json!([{"selector": DUPLICATES, "reason": ""}]),
                ),
            );
            assert_eq!(first["repair_count"], 1, "{first}");
            assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
            let mut input = no_diff_input(&workspace, &first["repair_prepared"], drops.clone());
            input["prior_applied_count"] = first["applied_count"].clone();
            input["carried_task_outcomes"] = first["non_repairable_outcomes"].clone();
            input
        } else {
            no_diff_input(&workspace, &prepared, drops.clone())
        };
        let applied = workspace.action("apply_task_pilot_results", input.clone());
        assert_eq!(
            applied["status"], "succeeded",
            "initial/repair={repair}: {applied}"
        );
        assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
        assert_eq!(applied["tasks"][0]["disposition"], "verified_no_diff");
        assert_eq!(applied["tasks"][0]["context_evidence_dropped"], drops);
        assert!(
            applied["tasks"][0]
                .get("context_evidence_retained")
                .is_none()
        );
        assert!(workspace.scope(&task.id).is_empty());
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().status,
            TaskStatus::Proposed
        );
        assert_eq!(
            workspace.action("pipeline_success_guard", json!({"result": applied}))["succeeded"],
            true
        );
        let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
        let audit: Value =
            super::pilot_audit(&workspace.runtime, &task.id, comments.last().unwrap());
        assert_eq!(audit["assessment"]["context_evidence_dropped"], drops);
        assert_eq!(audit["recheck_basis"]["source_revision"], pinned);
        assert_eq!(audit["context_files_before"], json!(FILED));
        assert_eq!(
            audit["assessment"]["already_landed"],
            input["results"][0]["tasks"][0]["already_landed"]
        );
        let replayed = workspace.action("apply_task_pilot_results", input);
        assert_eq!(
            outcome(&replayed)["outcome"],
            "already_applied",
            "{replayed}"
        );
        assert_eq!(
            workspace.runtime.get_task_comments(&task.id).unwrap().len(),
            comments.len()
        );
        assert_eq!(
            workspace.object("HEAD"),
            pinned,
            "no manufactured implementation commit"
        );
        assert!(workspace.git(&["status", "--porcelain"]).is_empty());
    }
}

crate::scenario_table!(
    verified_no_diff_evidence_drop_scenarios,
    verified_no_diff_accepts_evidence_drops_on_initial_and_targeted_repair_apply,
    verified_no_diff_drop_records_cannot_bypass_scope_or_inspection_proof,
    verified_no_diff_drop_records_cannot_revoke_creation_grants,
    verified_no_diff_drop_records_preserve_covering_proof_for_closure,
);

fn verified_no_diff_drop_records_preserve_covering_proof_for_closure() {
    for proved in [true, false] {
        let workspace = Workspace::new();
        workspace.commit_files(&FILED);
        let task = workspace.filed_task(
            "already satisfied review",
            &[
                "delivery-code-review",
                "ci-failure-sweep",
                "ci-failure:fixture-key",
            ],
            &FILED,
        );
        let prepared = workspace.prepare(&[&task.id]);
        let pinned = workspace.object("HEAD");
        let drops =
            json!([{"selector": DUPLICATES, "reason": "Inspected the already repaired source."}]);
        let mut input = no_diff_input(&workspace, &prepared, drops.clone());
        // An explicit already_landed finding uses its own existing hold.
        // Exercise the separate verified_no_diff covering-proof closure here.
        input["results"][0]["tasks"][0]["already_landed"] = Value::Null;
        if !proved {
            let evidence =
                json!("An unresolvable commit 0badc0ffee1 supposedly fixed the finding.");
            input["results"][0]["tasks"][0]["evidence"] = evidence;
        }
        input["ci_sweep_filing"] = json!({
            "task_id": task.id, "failure_key": "fixture-key", "tested_commit": "0123abcd",
            "workflow": "ci", "job": "test", "step": "cargo test",
            "run_urls": ["https://github.com/example/repo/actions/runs/1"],
        });
        input["promotion_authorized"] = json!(true);
        let applied = workspace.action("apply_task_pilot_results", input);
        assert_eq!(applied["status"], "succeeded", "{applied}");
        let decision = &applied["ci_sweep_admission"][0];
        assert_eq!(
            decision["classification"], "pilot_verified_no_diff",
            "{applied}"
        );
        assert_eq!(decision["closed"], proved, "{applied}");
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().status,
            if proved {
                TaskStatus::Archived
            } else {
                TaskStatus::Proposed
            }
        );
        if proved {
            assert_eq!(decision["covering_commits"], json!([pinned]));
        } else {
            assert!(
                decision["not_closed_reason"]
                    .as_str()
                    .unwrap()
                    .contains("not a commit"),
                "{applied}"
            );
        }
        assert_eq!(applied["tasks"][0]["context_evidence_dropped"], drops);
        assert_eq!(workspace.object("HEAD"), pinned);
        assert!(workspace.git(&["status", "--porcelain"]).is_empty());
    }
}

fn verified_no_diff_drop_records_cannot_bypass_scope_or_inspection_proof() {
    let workspace = Workspace::new();
    workspace.commit_files(&[DUPLICATES, EXTRA]);
    let missing = "file:src/removed.rs";
    let task = workspace.filed_task(
        "already satisfied review",
        &["code-review"],
        &[DUPLICATES, missing],
    );
    let prepared = workspace.prepare(&[&task.id]);
    let valid =
        json!([{"selector": DUPLICATES, "reason": "The pinned source is already correct."}]);
    let mut inputs = vec![
        no_diff_input(
            &workspace,
            &prepared,
            json!([{"selector": EXTRA, "reason": "Not in the prepared review scope."}]),
        ),
        no_diff_input(
            &workspace,
            &prepared,
            json!([{"selector": missing, "reason": "Does not resolve at the pinned source."}]),
        ),
    ];
    for field in ["observed_revision", "observed_tree"] {
        let mut forged =
            workspace.with_inspection_proof(no_diff_input(&workspace, &prepared, valid.clone()));
        forged["results"][0][field] = json!("0badc0ffee1");
        inputs.push(forged);
    }
    let mut unproved = no_diff_input(&workspace, &prepared, valid.clone());
    unproved["results"][0]["tasks"][0]["evidence"] = json!("");
    inputs.push(unproved);
    let mut target = no_diff_input(&workspace, &prepared, valid);
    target["results"][0]["tasks"][0]["disposition"] = json!("selectors");
    target["results"][0]["tasks"][0]["context_files_after"] = json!([DUPLICATES]);
    inputs.push(target);
    let history = workspace.runtime.get_task_history(&task.id).unwrap().len();
    for input in inputs {
        let applied = workspace.action("apply_task_pilot_results", input);
        assert_eq!(applied["applied_count"], 0, "{applied}");
        assert_eq!(applied["repair_count"], 1, "{applied}");
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap().len(),
            history
        );
        assert!(
            workspace
                .runtime
                .get_task_comments(&task.id)
                .unwrap()
                .is_empty()
        );
    }
}

fn verified_no_diff_drop_records_cannot_revoke_creation_grants() {
    for landed in [false, true] {
        let workspace = Workspace::new();
        let creation = "file:src/created.rs";
        let task = workspace
            .runtime
            .add_task_screened(
                TaskAddParams {
                    title: "review requires creation".into(),
                    description: "Create the missing module.".into(),
                    acceptance_criteria: vec!["The module exists.".into()],
                    plan: "Create the module.".into(),
                    status: Some(TaskStatus::Proposed),
                    tags: owned(&["delivery-code-review"]),
                    context_files: owned(&[creation]),
                    ..Default::default()
                },
                &TaskWriteScreen::unbound(true),
                None,
                None,
            )
            .unwrap()
            .task;
        if landed {
            workspace.commit_files(&[creation]);
        }
        let prepared = workspace.prepare(&[&task.id]);
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        let applied = workspace.action(
            "apply_task_pilot_results",
            no_diff_input(
                &workspace,
                &prepared,
                json!([{"selector": creation, "reason": "Claimed read-only."}]),
            ),
        );
        assert_eq!(outcome(&applied)["outcome"], "invalid", "{applied}");
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
    }
}

fn evidence_that_no_longer_resolves_is_dropped_and_reported() {
    let workspace = Workspace::new();
    workspace.commit_files(&FILED);
    let task = workspace.filed_task("duplicate alert", &["code-review"], &FILED);
    workspace.git(&["rm", "-q", CODE_GROUPS.trim_start_matches("file:")]);
    workspace.git(&["commit", "-m", "remove code groups"]);

    // Retention never keeps a selector apply would refuse from the pilot:
    // the deleted source is dropped and named for reauthorization.
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        applied["tasks"][0]["context_evidence_retained"],
        json!([DUPLICATES, FILING])
    );
    let findings = applied["tasks"][0]["context_reauthorization_required"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{applied}");
    assert!(findings[0].as_str().unwrap().contains(CODE_GROUPS));
    assert_eq!(
        workspace.scope(&task.id),
        [DUPLICATES, FILING, ADMISSION, SWEEP_TEST]
    );

    // Proposing the deleted source itself is still refused.
    let prepared = workspace.prepare(&[&task.id]);
    let refused = workspace.assess(&prepared, &[ADMISSION, CODE_GROUPS], "medium");
    assert_eq!(outcome(&refused)["outcome"], "invalid", "{refused}");
}

fn a_task_no_review_filed_keeps_the_pilot_narrowing() {
    let workspace = Workspace::new();
    workspace.commit_files(&FILED);
    let task = workspace.filed_task("ordinary", &["friction-curation"], &FILED);

    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert!(
        applied["tasks"][0]
            .get("context_evidence_retained")
            .is_none()
    );
    assert_eq!(workspace.scope(&task.id), NARROWED);
}

fn a_swept_proposal_is_applied_with_an_over_attachment_finding() {
    let workspace = Workspace::new();
    let swept = (0..11)
        .map(|index| format!("file:src/swept/module_{index}.rs"))
        .collect::<Vec<_>>();
    let swept = swept.iter().map(String::as_str).collect::<Vec<_>>();
    workspace.commit_files(&[&FILED[..], &swept].concat());

    // Over the low-tier budget: applied, with the finding attached.
    let ordinary = workspace.filed_task("swept", &[], &[ADMISSION]);
    let prepared = workspace.prepare(&[&ordinary.id]);
    let applied = workspace.assess(&prepared, &swept, "low");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    let findings = applied["tasks"][0]["context_attachment_warnings"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{applied}");
    assert_eq!(workspace.scope(&ordinary.id), owned(&swept));

    // Retained evidence counts toward the budget it reserves.
    let review = workspace.filed_task("review sweep", &["security-review"], &FILED);
    let prepared = workspace.prepare(&[&review.id]);
    let applied = workspace.assess(&prepared, &swept[..6], "low");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(workspace.scope(&review.id).len(), 11);
    assert_eq!(
        applied["tasks"][0]["context_attachment_warnings"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{applied}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    root_selectors_apply_at_the_pinned_source_scenarios,
    root_selectors_apply_at_the_pinned_source,
    omitted_root_is_retained_as_review_evidence_and_is_never_missing,
    root_assessments_preserve_pinned_path_confinement_and_kind_checks,
    review_evidence_survives_narrowing_reassessment_and_replay,
    verified_read_only_evidence_can_leave_the_lock_while_change_targets_remain,
    invalid_evidence_drop_records_cannot_narrow_the_scope,
    evidence_that_no_longer_resolves_is_dropped_and_reported,
    a_task_no_review_filed_keeps_the_pilot_narrowing,
    a_swept_proposal_is_applied_with_an_over_attachment_finding
);
