//! Operator-declared creation targets survive task-pilot preparation and
//! apply, while every missing selector the operator did not declare through
//! `allow_missing_context` is still refused or reported for reauthorization.
//! Fixtures drive the real prepare and apply actions against a real store and
//! a real repository, in isolated children.

use orbit_core::application::task::{
    TaskAddParams, TaskUpdateAuthority, TaskUpdateParams, TaskWriteScreen,
};
use orbit_core::{Task, TaskStatus};
use orbit_types::task::TaskArtifact;
use serde_json::{Value, json};

use super::{Workspace, runtime_at};

/// Existing material plus new module, test and directory targets, the shape
/// of a task that adds a module beside the code it changes.
const EXISTING: &str = "file:README.md";
const NEW_MODULE: &str = "file:src/recovery_decision.rs";
const NEW_TEST: &str = "file:tests/step_recovery.rs";
const NEW_DIR: &str = "dir:src/review";

impl Workspace {
    /// A proposed task over `context_files`, with every missing selector in
    /// `authorized` declared the way an operator surface declares it.
    fn scoped_task(&self, title: &str, context_files: &[&str], authorized: bool) -> Task {
        let params = TaskAddParams {
            title: title.into(),
            description: format!("Prepare {title}."),
            acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            context_files: owned(context_files),
            ..Default::default()
        };
        // An undeclared missing selector is the shape an internal writer or an
        // older release stored, which the permissive core add still accepts.
        if authorized {
            self.runtime
                .add_task_screened(params, &TaskWriteScreen::unbound(true), None, None)
                .unwrap()
                .task
        } else {
            self.runtime.add_task(params).unwrap()
        }
    }

    /// Replace `task`'s scope through the operator update path, declaring
    /// missing selectors when `allow_missing` is set and otherwise applying
    /// the strict check every surface runs.
    fn rescope(&self, task_id: &str, context_files: &[&str], allow_missing: bool) -> Task {
        self.runtime
            .update_task_screened(
                task_id,
                TaskUpdateParams {
                    context_files: Some(owned(context_files)),
                    ..Default::default()
                },
                &TaskWriteScreen::unbound(allow_missing),
                TaskUpdateAuthority::Lifecycle,
                Some("codex".into()),
                Some("fixture-model".into()),
            )
            .unwrap()
            .task
    }

    /// Apply one agent assessment for the single task `prepared` holds.
    fn apply_one(&self, prepared: &Value, before: &[String], after: &[&str]) -> Value {
        self.apply_disposition(prepared, before, after, "selectors")
    }

    fn apply_disposition(
        &self,
        prepared: &Value,
        before: &[String],
        after: &[&str],
        disposition: &str,
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
                        "task_id": task_id,
                        "context_files_before": before, "context_files_after": after,
                        "disposition": disposition, "recommended_crew": "fixture",
                        "recommended_complexity": "low", "confidence": "high",
                        "assessment_rationale": "README.md contains the affected material.",
                        "validation_approach": "Inspect the persisted scope.",
                        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                        "duplicate_of": null, "already_landed": null,
                    }],
                }],
            }),
        )
    }

    fn commit(&self, path: &str, content: &str) {
        let target = self.repo.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, content).unwrap();
        self.git(&["add", "."]);
        self.git(&["commit", "-m", path]);
    }
}

fn owned(selectors: &[&str]) -> Vec<String> {
    selectors
        .iter()
        .map(|selector| selector.to_string())
        .collect()
}

fn sorted(mut selectors: Vec<String>) -> Vec<String> {
    selectors.sort();
    selectors
}

fn outcome(applied: &Value) -> &Value {
    &applied["task_outcomes"][0]
}

fn apply_creation_landed(
    workspace: &Workspace,
    prepared: &Value,
    task: &Task,
    evidence: &str,
    retain_selectors: bool,
) -> Value {
    let after = if retain_selectors {
        json!(task.context_files)
    } else {
        json!([])
    };
    workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [{
                "task_id": task.id, "context_files_after": after,
                "disposition": if retain_selectors { "selectors" } else { "verified_no_diff" },
                "evidence": evidence, "already_landed": {"evidence": evidence},
                "recommended_crew": "fixture", "recommended_complexity": "low",
                "confidence": "high", "assessment_rationale": "All requested scope is delivered.",
                "validation_approach": "Check covering commits and retained authority.",
                "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                "duplicate_of": null,
            }]}],
        }),
    )
}

fn creation_landed_requires_present_targets_and_complete_pinned_covering_proof() {
    let workspace = Workspace::new();
    let task = workspace.scoped_task(
        "delivered creation",
        &[EXISTING, NEW_MODULE, NEW_TEST],
        true,
    );
    let readme = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    let before_creation = workspace.prepare(&[&task.id]);
    // Checkout-only files and later base commits cannot satisfy a pinned
    // inspection where the creation targets were absent.
    workspace.commit("src/recovery_decision.rs", "pub fn delivered() {}\n");
    let module = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    let one_missing = workspace.prepare(&[&task.id]);
    workspace.commit("tests/step_recovery.rs", "// delivered test\n");
    let test = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    let complete = workspace.prepare(&[&task.id]);
    let evidence = format!("Delivered by {readme}, {module}, and {test}.");
    for prepared in [&before_creation, &one_missing] {
        for retain_selectors in [false, true] {
            let applied =
                apply_creation_landed(&workspace, prepared, &task, &evidence, retain_selectors);
            assert_eq!(outcome(&applied)["outcome"], "invalid", "{applied}");
            assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
        }
    }
    workspace.git(&["checkout", "-b", "side"]);
    workspace.commit("src/recovery_decision.rs", "pub fn side() {}\n");
    let side = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    workspace.git(&["checkout", "main"]);
    workspace.commit("unrelated.md", "unrelated\n");
    let unrelated = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    workspace.commit("src/recovery_decision.rs", "pub fn later() {}\n");
    let later = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    for evidence in [
        "No covering commit cited.".to_string(),
        format!("Only existing material: {readme}."),
        format!("Only creation scope: {module} and {test}."),
        format!("One target uncovered: {readme} and {module}."),
        format!("Unknown citation: {readme}, {module}, {test}, 0badc0ffee1."),
        format!("Off-base target proof: {readme}, {side}, {test}."),
        format!("Unrelated proof: {unrelated}."),
        format!("Only later target proof: {readme}, {later}, {test}."),
    ] {
        for retain_selectors in [false, true] {
            let applied =
                apply_creation_landed(&workspace, &complete, &task, &evidence, retain_selectors);
            assert_eq!(
                outcome(&applied)["outcome"],
                "invalid",
                "{evidence}: {applied}"
            );
            assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
        }
    }
    let applied = apply_creation_landed(&workspace, &complete, &task, &evidence, false);
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(applied["tasks"][0]["disposition"], "selectors");
    assert_eq!(
        applied["tasks"][0]["already_landed"]["operator_reconciliation_required"],
        true
    );
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().context_files,
        task.context_files
    );
    let replay = apply_creation_landed(&workspace, &complete, &task, &evidence, false);
    assert_eq!(outcome(&replay)["outcome"], "already_applied", "{replay}");
    // A culprit citation is not a covering delivery, even if it changes the
    // target and every other path has proof.
    let task = workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                description: Some(format!("Repair the defect introduced by {module}.")),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let applied = apply_creation_landed(&workspace, &prepared, &task, &evidence, false);
    assert_eq!(outcome(&applied)["outcome"], "invalid", "{applied}");
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    // The failed finding must not poison ordinary preparation of unfulfilled work.
    let applied = workspace.apply_one(
        &prepared,
        &task.context_files,
        &[EXISTING, NEW_MODULE, NEW_TEST],
    );
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
}

fn declared_creation_targets_survive_prepare_apply_restart_and_replay() {
    let workspace = Workspace::new();
    let task = workspace.scoped_task(
        "mixed scope",
        &[EXISTING, NEW_MODULE, NEW_TEST, NEW_DIR],
        true,
    );
    let declared = sorted(owned(&[NEW_MODULE, NEW_TEST, NEW_DIR]));

    let prepared = workspace.prepare(&[&task.id]);
    let snapshot = &prepared["tasks"][0];
    assert_eq!(snapshot["context_creation_selectors"], json!(declared));
    assert!(
        snapshot["context_creation_identity"].is_string(),
        "{snapshot}"
    );

    // The agent cannot see the grant and keeps only the module: the host
    // keeps the targets it omitted rather than losing the operator's intent.
    let applied = workspace.apply_one(&prepared, &task.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(outcome(&applied)["outcome"], "applied");
    assert_eq!(
        applied["tasks"][0]["context_creation_retained"],
        json!(sorted(owned(&[NEW_TEST, NEW_DIR])))
    );
    assert!(
        applied["tasks"][0]
            .get("context_reauthorization_required")
            .is_none()
    );
    let stored = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(
        sorted(stored.context_files.clone()),
        sorted(owned(&[EXISTING, NEW_MODULE, NEW_TEST, NEW_DIR]))
    );

    // Replaying the same apply settles as already applied.
    let replayed = workspace.apply_one(&prepared, &task.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(
        outcome(&replayed)["outcome"],
        "already_applied",
        "{replayed}"
    );

    // A reopened runtime still reads the grant the pilot's own write carried,
    // so a later pass over the rewritten scope prepares and applies cleanly.
    let restarted = runtime_at(
        &workspace.runtime.global_root(),
        &workspace.repo.join(".orbit"),
    );
    let reread = restarted.get_task(&task.id).unwrap();
    assert_eq!(reread, stored);
    let again = workspace.prepare(&[&task.id]);
    assert_eq!(
        again["tasks"][0]["context_creation_selectors"],
        json!(declared)
    );
    let reapplied = workspace.apply_one(
        &again,
        &stored.context_files,
        &[EXISTING, NEW_MODULE, NEW_TEST, NEW_DIR],
    );
    assert_eq!(outcome(&reapplied)["outcome"], "applied", "{reapplied}");

    // A dropping disposition cannot discard operator intent either.
    let third = workspace.prepare(&[&task.id]);
    let before = workspace.runtime.get_task(&task.id).unwrap().context_files;
    let dropped = workspace.apply_disposition(&third, &before, &[], "verified_no_diff");
    assert_eq!(outcome(&dropped)["outcome"], "invalid", "{dropped}");
    assert_eq!(
        sorted(workspace.runtime.get_task(&task.id).unwrap().context_files),
        sorted(owned(&[EXISTING, NEW_MODULE, NEW_TEST, NEW_DIR]))
    );
}

fn undeclared_missing_selectors_stay_refused() {
    let workspace = Workspace::new();
    let declared = workspace.scoped_task("declared", &[EXISTING, NEW_MODULE], true);

    // Each proposal the grant does not cover is invalid: a novel missing
    // path, a near-miss typo of the declared one, a traversal, and a path
    // that only appears in a commit after the pinned source revision.
    let prepared = workspace.prepare(&[&declared.id]);
    workspace.commit("src/later.rs", "fn later() {}\n");
    for proposal in [
        "file:src/invented.rs",
        "file:src/recovery_decisions.rs",
        "file:../outside.rs",
        "file:src/later.rs",
    ] {
        let applied =
            workspace.apply_one(&prepared, &declared.context_files, &[EXISTING, proposal]);
        assert_eq!(
            outcome(&applied)["outcome"],
            "invalid",
            "{proposal}: {applied}"
        );
        assert_eq!(applied["repair_count"], 1, "{proposal}");
    }
    assert_eq!(workspace.runtime.get_task(&declared.id).unwrap(), declared);

    // A grant copied onto another task is not that task's grant: the other
    // task's selector is undeclared there.
    let other = workspace.scoped_task("other", &[EXISTING, NEW_MODULE], false);
    let prepared = workspace.prepare(&[&other.id]);
    assert_eq!(
        prepared["tasks"][0]["context_creation_selectors"],
        json!([])
    );
    let applied = workspace.apply_one(&prepared, &other.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(outcome(&applied)["outcome"], "invalid", "{applied}");

    // A declared target that became a directory or a symlink at the source
    // revision fails the kind check instead of being accepted as missing.
    let mut changed_kind = vec!["file:src/kind.rs"];
    if cfg!(unix) {
        changed_kind.push("file:src/link.rs");
    }
    let kinded = workspace.scoped_task("kinded", &[&[EXISTING][..], &changed_kind].concat(), true);
    workspace.commit("src/kind.rs/inner.rs", "fn inner() {}\n");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../README.md", workspace.repo.join("src/link.rs")).unwrap();
        workspace.git(&["add", "."]);
        workspace.git(&["commit", "-m", "link"]);
    }
    let prepared = workspace.prepare(&[&kinded.id]);
    for proposal in changed_kind {
        let applied = workspace.apply_one(&prepared, &kinded.context_files, &[EXISTING, proposal]);
        assert_eq!(
            outcome(&applied)["outcome"],
            "invalid",
            "{proposal}: {applied}"
        );
    }
}

fn legacy_missing_target_is_reported_and_reauthorized_through_task_update() {
    let workspace = Workspace::new();
    // A record written before grants existed: the missing selector carries
    // no durable authorization, and nothing infers one.
    let legacy = workspace.scoped_task("legacy", &[EXISTING, NEW_MODULE], false);
    let prepared = workspace.prepare(&[&legacy.id]);
    let kept = workspace.apply_one(&prepared, &legacy.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(outcome(&kept)["outcome"], "invalid", "{kept}");
    let error = outcome(&kept)["error"].as_str().unwrap();
    assert!(error.contains("allow_missing_context"), "{error}");

    // The targeted repair drops it and says so, instead of silently losing
    // the target or looping on it.
    let repaired =
        workspace.apply_one(&kept["repair_prepared"], &legacy.context_files, &[EXISTING]);
    assert_eq!(repaired["status"], "succeeded", "{repaired}");
    let findings = repaired["tasks"][0]["context_reauthorization_required"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{repaired}");
    assert!(findings[0].as_str().unwrap().contains(NEW_MODULE));
    assert_eq!(
        workspace
            .runtime
            .get_task(&legacy.id)
            .unwrap()
            .context_files,
        [EXISTING]
    );

    // The operator re-declares it through the ordinary update, and the next
    // pass keeps it.
    let reauthorized = workspace.rescope(&legacy.id, &[EXISTING, NEW_MODULE], true);
    let prepared = workspace.prepare(&[&legacy.id]);
    assert_eq!(
        prepared["tasks"][0]["context_creation_selectors"],
        json!([NEW_MODULE])
    );
    let applied = workspace.apply_one(
        &prepared,
        &reauthorized.context_files,
        &[EXISTING, NEW_MODULE],
    );
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");

    // A strict re-send keeps the grant; replacing the scope without the
    // target revokes it, and re-adding it strictly is refused.
    workspace.rescope(&legacy.id, &[EXISTING, NEW_MODULE], false);
    workspace.rescope(&legacy.id, &[EXISTING], false);
    let refused = workspace.runtime.update_task_screened(
        &legacy.id,
        TaskUpdateParams {
            context_files: Some(owned(&[EXISTING, NEW_MODULE])),
            ..Default::default()
        },
        &TaskWriteScreen::unbound(false),
        TaskUpdateAuthority::Lifecycle,
        Some("codex".into()),
        Some("fixture-model".into()),
    );
    assert!(refused.is_err(), "a revoked grant must not revive");
}

fn creation_grant_changed_after_preparation_supersedes_the_write() {
    let workspace = Workspace::new();
    let task = workspace.scoped_task("legacy", &[EXISTING, NEW_MODULE], false);
    let prepared = workspace.prepare(&[&task.id]);
    assert!(prepared["tasks"][0]["context_creation_identity"].is_null());

    // An operator declares the target while the pilot assesses the task:
    // the scope is unchanged, but the authorization the preparation saw is
    // not the task's any more, so the write is stale rather than applied
    // under intent the preparation never captured.
    workspace.rescope(&task.id, &[EXISTING, NEW_MODULE], true);
    let applied = workspace.apply_one(&prepared, &task.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(outcome(&applied)["outcome"], "superseded", "{applied}");
    assert_eq!(applied["status"], "succeeded");
    assert_eq!(outcome(&applied)["reason"], "context_creation_changed");
    let after = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(after.context_files, task.context_files);
    assert_eq!(after.complexity, task.complexity);

    // A fresh preparation applies.
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.apply_one(&prepared, &after.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
}

fn revoke_and_same_scope_reauthorization_supersedes_an_older_preparation() {
    let workspace = Workspace::new();
    let task = workspace.scoped_task("declared", &[EXISTING, NEW_MODULE], true);
    let prepared = workspace.prepare(&[&task.id]);
    let prior_identity = prepared["tasks"][0]["context_creation_identity"].clone();
    assert!(prior_identity.is_string());

    workspace.rescope(&task.id, &[EXISTING], false);
    workspace.rescope(&task.id, &[EXISTING, NEW_MODULE], true);
    let current = workspace.runtime.get_task(&task.id).unwrap();
    let applied = workspace.apply_one(&prepared, &task.context_files, &[EXISTING, NEW_MODULE]);

    assert_eq!(outcome(&applied)["outcome"], "superseded", "{applied}");
    assert_eq!(applied["status"], "succeeded");
    assert_eq!(outcome(&applied)["reason"], "context_creation_changed");
    assert_eq!(current.context_files, task.context_files);
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), current);
}

fn benign_comment_and_artifact_writes_preserve_preparation() {
    let workspace = Workspace::new();
    let task = workspace.scoped_task("declared", &[EXISTING, NEW_MODULE], true);
    let prepared = workspace.prepare(&[&task.id]);
    let identity = prepared["tasks"][0]["context_creation_identity"].clone();
    assert!(identity.is_string());

    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("A benign comment during assessment.".to_string()),
                ..Default::default()
            },
            Some("codex".into()),
            Some("fixture-model".into()),
        )
        .unwrap();
    let after_comment = workspace.prepare(&[&task.id]);
    assert_eq!(
        after_comment["tasks"][0]["context_creation_identity"], identity,
        "history-only updates preserve the semantic creation grant"
    );

    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "notes/review.txt",
                    "A benign artifact during assessment.",
                )],
                ..Default::default()
            },
            Some("codex".into()),
            Some("fixture-model".into()),
        )
        .unwrap();
    let after_artifact = workspace.prepare(&[&task.id]);
    assert_eq!(
        after_artifact["tasks"][0]["context_creation_identity"], identity,
        "artifact-only updates preserve the semantic creation grant"
    );

    let applied = workspace.apply_one(&prepared, &task.context_files, &[EXISTING, NEW_MODULE]);
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    declared_creation_targets_survive_prepare_apply_restart_and_replay_scenarios,
    creation_landed_requires_present_targets_and_complete_pinned_covering_proof,
    declared_creation_targets_survive_prepare_apply_restart_and_replay,
    undeclared_missing_selectors_stay_refused,
    legacy_missing_target_is_reported_and_reauthorized_through_task_update,
    creation_grant_changed_after_preparation_supersedes_the_write,
    revoke_and_same_scope_reauthorization_supersedes_an_older_preparation,
    benign_comment_and_artifact_writes_preserve_preparation
);
