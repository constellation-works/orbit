//! Operator-declared creation targets survive task-pilot preparation and
//! apply, while every missing selector the operator did not declare through
//! `allow_missing_context` is still refused or reported for reauthorization.
//! Fixtures drive the real prepare and apply actions against a real store and
//! a real repository, in isolated children.

use orbit_core::application::task::{
    TaskAddParams, TaskUpdateAuthority, TaskUpdateParams, TaskWriteScreen,
};
use orbit_types::task::TaskArtifact;
use orbit_types::task::{Task, TaskStatus};
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
        self.apply_with(prepared, before, after, disposition, json!({}))
    }

    /// [`Self::apply_disposition`] with `extra` assessment fields merged in. A
    /// non-`selectors` disposition carries the evidence the schema requires.
    fn apply_with(
        &self,
        prepared: &Value,
        before: &[String],
        after: &[&str],
        disposition: &str,
        extra: Value,
    ) -> Value {
        let task_id = prepared["task_ids"][0].clone();
        let mut assessment = json!({
            "task_id": task_id,
            "context_files_before": before, "context_files_after": after,
            "disposition": disposition, "recommended_crew": "fixture",
            "recommended_complexity": "low", "confidence": "high",
            "assessment_rationale": "README.md contains the affected material.",
            "validation_approach": "Inspect the persisted scope.",
            "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
            "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
            "duplicate_of": null, "already_landed": null,
        });
        if disposition != "selectors" {
            assessment["evidence"] = json!("The deliverable needs no repository change.");
        }
        for (key, value) in extra.as_object().into_iter().flatten() {
            assessment[key] = value.clone();
        }
        self.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": self.repo,
                "prepared": prepared,
                "results": [{
                    "partition_index": 0, "task_ids": [task_id],
                    "tasks": [assessment],
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

fn apply_creation_landed_split(
    workspace: &Workspace,
    prepared: &Value,
    task: &Task,
    top: Option<&str>,
    nested: &str,
) -> Value {
    let mut assessment = json!({
        "task_id": task.id, "context_files_after": task.context_files,
        "disposition": "selectors", "already_landed": {"evidence": nested},
        "recommended_crew": "fixture", "recommended_complexity": "low",
        "confidence": "high", "assessment_rationale": "All requested scope is delivered.",
        "validation_approach": "Check covering commits and retained authority.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null,
    });
    if let Some(evidence) = top {
        assessment["evidence"] = json!(evidence);
    }
    workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        }),
    )
}

fn complete_landed_setup() -> (Workspace, Task, Value, String) {
    let workspace = Workspace::new();
    let task = workspace.scoped_task(
        "delivered creation",
        &[EXISTING, NEW_MODULE, NEW_TEST],
        true,
    );
    let readme = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    workspace.commit("src/recovery_decision.rs", "pub fn delivered() {}\n");
    let module = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    workspace.commit("tests/step_recovery.rs", "// delivered test\n");
    let test = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
    let complete = workspace.prepare(&[&task.id]);
    let evidence = format!("Delivered by {readme}, {module}, and {test}.");
    (workspace, task, complete, evidence)
}

fn creation_landed_uses_nested_evidence_when_top_level_is_blank() {
    // A blank optional top-level `evidence` must not hide complete pinned
    // covering proof in `already_landed.evidence` (ORB-19803).
    for top in [None, Some(""), Some("   "), Some("\n \t ")] {
        let (workspace, task, complete, evidence) = complete_landed_setup();
        let applied = apply_creation_landed_split(&workspace, &complete, &task, top, &evidence);
        assert_eq!(
            outcome(&applied)["outcome"],
            "applied",
            "{top:?}: {applied}"
        );
        assert_eq!(applied["tasks"][0]["disposition"], "selectors", "{applied}");
        assert_eq!(
            applied["tasks"][0]["already_landed"]["operator_reconciliation_required"],
            json!(true),
            "{applied}"
        );
        let normalized = evidence.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            applied["tasks"][0]["already_landed"]["evidence"],
            json!(normalized),
            "{applied}"
        );
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().context_files,
            task.context_files,
            "{applied}"
        );
    }

    // Genuinely missing evidence and insufficient covering proof stay refused.
    let (workspace, task, complete, evidence) = complete_landed_setup();
    for (top, nested) in [
        (None, ""),
        (Some(""), ""),
        (Some("   "), "   "),
        (None, "No covering commit cited."),
    ] {
        let applied = apply_creation_landed_split(&workspace, &complete, &task, top, nested);
        assert_eq!(
            outcome(&applied)["outcome"],
            "invalid",
            "{top:?} / {nested:?}: {applied}"
        );
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    }
    let partial = {
        let readme = workspace.git(&["rev-parse", "HEAD~2"]).trim().to_string();
        let module = workspace.git(&["rev-parse", "HEAD~1"]).trim().to_string();
        format!("Only creation scope: {readme} and {module}.")
    };
    let _ = evidence;
    let applied = apply_creation_landed_split(&workspace, &complete, &task, Some(""), &partial);
    assert_eq!(outcome(&applied)["outcome"], "invalid", "{applied}");
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
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

#[cfg(unix)]
fn verify_pipeline_delivers_current_creation_selectors(
    workspace: &Workspace,
    task: &Task,
    expected_selectors: &[String],
) {
    use std::os::unix::fs::PermissionsExt;

    use chrono::Utc;
    use orbit_types::resource::ExecutorResource;
    use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, JobRunState, PipelineState};

    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let provider = workspace.root.path().join("codex");
    let seen = workspace.root.path().join("seen-creation-selectors.json");
    std::fs::write(
        &provider,
        format!(
            r#"#!/usr/bin/env python3
import json, subprocess, sys
raw = sys.stdin.read()
marker = "Execution envelope:\n"
idx = raw.rfind(marker)
if idx == -1:
    sys.stderr.write("missing execution envelope\n")
    sys.exit(1)
envelope = json.loads(raw[idx + len(marker):].strip())
partition = envelope["input"]["partition"]
task_id = partition["task_ids"][0]
creation = partition.get("context_creation_selectors", {{}})
with open({seen:?}, "w", encoding="utf-8") as handle:
    json.dump(creation, handle)
granted = creation.get(task_id)
if not granted:
    sys.stderr.write(f"missing partition.context_creation_selectors for {{task_id}}\n")
    sys.exit(1)
head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
tree = subprocess.check_output(["git", "rev-parse", "HEAD^{{tree}}"], text=True).strip()
sys.stdout.write(json.dumps({{
    "schemaVersion": 1,
    "status": "success",
    "error": None,
    "result": {{
        "partition_index": partition["partition_index"],
        "observed_revision": head,
        "observed_tree": tree,
        "task_ids": [task_id],
        "tasks": [{{
            "task_id": task_id,
            "context_files_after": ["file:README.md"] + granted,
            "disposition": "selectors",
            "recommended_complexity": "low",
            "confidence": "high",
            "assessment_rationale": "Kept current creation selectors from partition.context_creation_selectors.",
            "validation_approach": "Inspect the persisted scope.",
            "evidence_gaps": [],
            "reassessment_triggers": [],
            "blocked_by": [],
            "adr_conflicts": [],
            "utility_warnings": [],
            "surface_warnings": [],
            "duplicate_of": None,
            "already_landed": None,
        }}],
    }},
}}))
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    workspace.install_pilot_job();
    let run = workspace
        .jobs
        .insert_job_run(
            "task_pilot_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task.id],
                "workspace_path": workspace.repo,
                "base_branch": "main",
            })),
            None,
        )
        .unwrap();
    workspace
        .runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), "task_pilot_pipeline".into(), json!({})),
        )
        .unwrap();
    workspace
        .runtime
        .execute_pipeline_run_worker(&run.run_id)
        .unwrap_or_else(|error| panic!("pilot pipeline must finish: {error}"));
    let finished = workspace.runtime.show_job_run(&run.run_id).unwrap();
    assert_eq!(finished.state, JobRunState::Success, "{finished:?}");
    let observed: Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).expect("seen json");
    assert_eq!(observed[&task.id], json!(expected_selectors));
    assert_eq!(
        sorted(workspace.runtime.get_task(&task.id).unwrap().context_files),
        sorted(owned(&[EXISTING, NEW_MODULE, NEW_TEST]))
    );
}

#[cfg(not(unix))]
fn verify_pipeline_delivers_current_creation_selectors(
    _workspace: &Workspace,
    _task: &Task,
    _expected_selectors: &[String],
) {
}

fn newer_creation_grant_reaches_partition_and_ungranted_missing_path_stays_gap() {
    let workspace = Workspace::new();
    // Older grant names only the module; newer grant adds the test target.
    let task = workspace.scoped_task("multi-grant", &[EXISTING, NEW_MODULE], true);
    let updated = workspace.rescope(&task.id, &[EXISTING, NEW_MODULE, NEW_TEST], true);
    let expected = sorted(owned(&[NEW_MODULE, NEW_TEST]));

    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(
        prepared["tasks"][0]["context_creation_selectors"],
        json!(expected),
        "{prepared}"
    );
    assert_eq!(
        prepared["partitions"][0]["context_creation_selectors"][&task.id],
        json!(expected),
        "partition must carry current creation selectors for the pilot agent: {prepared}"
    );

    // An ungranted missing path is absent from the partition grant and refused
    // if proposed; repair_partitions keeps the task's current creation selectors.
    let ungranted = "file:src/ungranted.rs";
    let refused = workspace.apply_one(
        &prepared,
        &updated.context_files,
        &[EXISTING, NEW_MODULE, NEW_TEST, ungranted],
    );
    assert_eq!(outcome(&refused)["outcome"], "invalid", "{refused}");
    assert_eq!(refused["repair_count"], 1, "{refused}");
    assert_eq!(
        refused["repair_partitions"][0]["context_creation_selectors"][&task.id],
        json!(expected),
        "repair partition must retain current creation selectors: {refused}"
    );

    // Reporting the ungranted missing path as an evidence gap while keeping
    // the granted targets succeeds and records the gap.
    let gap = format!("{ungranted} is absent at source_revision and has no current grant.");
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": 0, "task_ids": [task.id],
                "tasks": [{
                    "task_id": task.id,
                    "context_files_after": [EXISTING, NEW_MODULE, NEW_TEST],
                    "disposition": "selectors", "recommended_crew": "fixture",
                    "recommended_complexity": "low", "confidence": "medium",
                    "assessment_rationale": "Granted targets kept; ungranted missing path reported.",
                    "validation_approach": "Inspect the persisted scope and evidence_gaps.",
                    "evidence_gaps": [gap],
                    "reassessment_triggers": [], "blocked_by": [],
                    "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                    "duplicate_of": null, "already_landed": null,
                }],
            }],
        }),
    );
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(applied["tasks"][0]["evidence_gaps"], json!([gap]));

    // A task whose grant was revoked carries no partition creation selectors.
    let revoked = workspace.scoped_task("revoked", &[EXISTING, NEW_MODULE], true);
    workspace.rescope(&revoked.id, &[EXISTING], false);
    let revoked_prepared = workspace.prepare(&[&revoked.id]);
    assert!(
        revoked_prepared["partitions"][0]
            .get("context_creation_selectors")
            .is_none(),
        "revoked grant must not appear on partition: {revoked_prepared}"
    );

    // Full pipeline delivery: the pilot worker receives the latest grant's
    // selectors on `partition.context_creation_selectors` and keeps all of them.
    let pipeline_task =
        workspace.scoped_task("pipeline multi-grant", &[EXISTING, NEW_MODULE], true);
    workspace.rescope(&pipeline_task.id, &[EXISTING, NEW_MODULE, NEW_TEST], true);
    verify_pipeline_delivers_current_creation_selectors(&workspace, &pipeline_task, &expected);
}

/// A bounded migration: verified existing sources the change deletes or edits
/// plus exact granted destinations. Retaining the grants is additive to the
/// pilot's own selection of the existing lock footprint, and when preparation
/// explicitly establishes required existing changes, omitting one refuses the
/// assessment with an actionable error while read-only omissions still pass.
fn migration_footprint_keeps_existing_targets_beside_granted_creations() {
    let workspace = Workspace::new();
    let old_scope = "file:docs/old/1_scope.md";
    let old_design = "file:docs/old/2_design.md";
    let read_only = "file:docs/reference.md";
    workspace.commit("docs/old/1_scope.md", "# scope\n");
    workspace.commit("docs/old/2_design.md", "# design\n");
    workspace.commit("docs/reference.md", "# reference\n");
    let task = workspace
        .runtime
        .add_task_screened(
            TaskAddParams {
                title: "migration".into(),
                description: format!(
                    "Migrate the old scope: delete {old_scope} and edit {old_design}. \
                     Consult {read_only} as read-only reference."
                ),
                acceptance_criteria: vec!["Selectors identify both sets.".into()],
                plan: "Inspect docs/old and docs/reference.md.".into(),
                status: Some(TaskStatus::Proposed),
                context_files: owned(&[
                    old_scope, old_design, read_only, NEW_MODULE, NEW_TEST, NEW_DIR,
                ]),
                ..Default::default()
            },
            &TaskWriteScreen::unbound(true),
            None,
            None,
        )
        .unwrap()
        .task;
    let creations = owned(&[NEW_MODULE, NEW_TEST, NEW_DIR]);
    let required_existing = sorted(owned(&[old_scope, old_design]));
    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(
        prepared["partitions"][0]["context_creation_selectors"][&task.id],
        json!(sorted(creations.clone())),
        "{prepared}"
    );
    assert_eq!(
        prepared["tasks"][0]["required_existing_selectors"],
        json!(required_existing),
        "{prepared}"
    );
    assert_eq!(
        prepared["partitions"][0]["required_existing_selectors"][&task.id],
        json!(required_existing),
        "{prepared}"
    );

    // Omitting all required existing targets (creations-only) is refused with
    // an actionable reason, leaves the stored task untouched, and preserves
    // both sets on the repair partition.
    let creations_only = workspace.apply_one(&prepared, &task.context_files, &[NEW_MODULE]);
    assert_eq!(
        outcome(&creations_only)["outcome"],
        "invalid",
        "{creations_only}"
    );
    assert_eq!(creations_only["repair_count"], 1, "{creations_only}");
    let error = outcome(&creations_only)["error"].as_str().unwrap();
    assert!(
        error.contains(old_scope) && error.contains(old_design),
        "refusal must name every omitted required existing target: {error}"
    );
    assert_eq!(
        creations_only["repair_partitions"][0]["required_existing_selectors"][&task.id],
        json!(required_existing),
        "{creations_only}"
    );
    assert_eq!(
        creations_only["repair_partitions"][0]["context_creation_selectors"][&task.id],
        json!(sorted(creations.clone())),
        "{creations_only}"
    );
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);

    // Omitting even one required existing target is refused, naming only the
    // missing required target.
    let missing_one = workspace.apply_one(
        &prepared,
        &task.context_files,
        &[old_scope, NEW_MODULE, NEW_TEST, NEW_DIR],
    );
    assert_eq!(outcome(&missing_one)["outcome"], "invalid", "{missing_one}");
    let partial_error = outcome(&missing_one)["error"].as_str().unwrap();
    assert!(partial_error.contains(old_design), "{partial_error}");
    assert!(!partial_error.contains(old_scope), "{partial_error}");
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);

    // A non-selectors disposition cannot drop required existing changes either.
    let dropped_disposition =
        workspace.apply_disposition(&prepared, &task.context_files, &[], "verified_no_diff");
    assert_eq!(
        outcome(&dropped_disposition)["outcome"],
        "invalid",
        "{dropped_disposition}"
    );
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);

    // An ungranted missing path alongside the required existing targets still refuses.
    let ungranted = workspace.apply_one(
        &prepared,
        &task.context_files,
        &[
            old_scope,
            old_design,
            NEW_MODULE,
            "file:src/invented_target.rs",
        ],
    );
    assert_eq!(outcome(&ungranted)["outcome"], "invalid", "{ungranted}");
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);

    // Repair succeeds when the pilot returns both required existing targets
    // and creation grants while deliberately omitting the read-only reference.
    let applied = workspace.apply_one(
        &creations_only["repair_prepared"],
        &task.context_files,
        &[old_scope, old_design, NEW_MODULE, NEW_TEST, NEW_DIR],
    );
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert!(
        applied["tasks"][0]
            .get("context_creation_retained")
            .is_none()
    );
    let stored = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(
        sorted(stored.context_files.clone()),
        sorted(owned(&[
            old_scope, old_design, NEW_MODULE, NEW_TEST, NEW_DIR
        ]))
    );
}

/// Covers ORB-29753-shaped migration specs, valid `file:` -> `symbol:` and
/// non-required narrowing, and creation-only work.
fn migration_contract_preserves_narrowing_read_only_omissions_and_creation_only_work() {
    let workspace = Workspace::new();
    let scope_doc = "file:docs/design/alpha/1_scope.md";
    let preset = "file:crates/orbit-core/src/app/workspace/preset.rs";
    let features = "file:docs/design/FEATURES.md";
    let reference = "file:docs/standards/exemplars.md";
    let new_overview = "file:docs/design/alpha/1_overview.md";
    workspace.commit("docs/design/alpha/1_scope.md", "# alpha scope\n");
    workspace.commit(
        "crates/orbit-core/src/app/workspace/preset.rs",
        "pub fn preset_fn() {}\n",
    );
    workspace.commit("docs/design/FEATURES.md", "# features\n");
    workspace.commit("docs/standards/exemplars.md", "# exemplars\n");

    // 1. ORB-29753 shape: listed feature scope deletion + explicitly named
    //    existing paths requiring deletion/updates/regeneration, beside a
    //    preserved/read-only path and an exact creation grant.
    let migration = workspace
        .runtime
        .add_task_screened(
            TaskAddParams {
                title: "Migrate alpha feature scope".into(),
                description: "\
This slice creates 1 exact file, relative to docs/design/<feature>/:
- alpha: 1_overview.md.

For each listed feature, migrate 1_scope.md into its overview and design. Delete the scope file and update live inbound links. Preserve vendored standards in docs/standards/exemplars.md. Paths that already exist on the integration tree and need deletion, inbound-reference updates or regeneration include crates/orbit-core/src/app/workspace/preset.rs, docs/design/FEATURES.md."
                    .into(),
                acceptance_criteria: vec!["Both sets remain locked.".into()],
                plan: "Migrate alpha.".into(),
                status: Some(TaskStatus::Proposed),
                context_files: owned(&[scope_doc, preset, features, reference, new_overview]),
                ..Default::default()
            },
            &TaskWriteScreen::unbound(true),
            None,
            None,
        )
        .unwrap()
        .task;
    let prepared = workspace.prepare(&[&migration.id]);
    assert_eq!(
        prepared["tasks"][0]["required_existing_selectors"],
        json!(sorted(owned(&[scope_doc, preset, features]))),
        "must require existing targets from both the feature-scope migration and named paths, \
         while leaving preserved/read-only docs/standards/exemplars.md out: {prepared}"
    );
    // Omitting existing targets fails; narrowing `preset` to `symbol:` passes
    // while omitting `reference`.
    let creations_only = workspace.apply_one(&prepared, &migration.context_files, &[new_overview]);
    assert_eq!(
        outcome(&creations_only)["outcome"],
        "invalid",
        "{creations_only}"
    );
    let narrowed_preset = "symbol:crates/orbit-core/src/app/workspace/preset.rs#preset_fn:function";
    let narrowed = workspace.apply_one(
        &prepared,
        &migration.context_files,
        &[scope_doc, narrowed_preset, features, new_overview],
    );
    assert_eq!(outcome(&narrowed)["outcome"], "applied", "{narrowed}");
    assert_eq!(
        sorted(
            workspace
                .runtime
                .get_task(&migration.id)
                .unwrap()
                .context_files
        ),
        sorted(owned(&[scope_doc, narrowed_preset, features, new_overview]))
    );

    // 2. Non-required existing selectors still allow valid narrowing.
    let narrowable = workspace.scoped_task("narrowable", &[features, reference, NEW_MODULE], true);
    let prepared = workspace.prepare(&[&narrowable.id]);
    assert!(
        prepared["tasks"][0]
            .get("required_existing_selectors")
            .is_none(),
        "{prepared}"
    );
    let applied = workspace.apply_one(&prepared, &narrowable.context_files, &[NEW_MODULE]);
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        workspace
            .runtime
            .get_task(&narrowable.id)
            .unwrap()
            .context_files,
        [NEW_MODULE]
    );

    // 3. Creation-only work remains supported without existing targets.
    let creation_only = workspace.scoped_task("creation-only", &[NEW_MODULE, NEW_TEST], true);
    let prepared = workspace.prepare(&[&creation_only.id]);
    assert!(
        prepared["tasks"][0]
            .get("required_existing_selectors")
            .is_none(),
        "{prepared}"
    );
    let applied = workspace.apply_one(&prepared, &creation_only.context_files, &[NEW_MODULE]);
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        sorted(
            workspace
                .runtime
                .get_task(&creation_only.id)
                .unwrap()
                .context_files
        ),
        sorted(owned(&[NEW_MODULE, NEW_TEST]))
    );
}

/// A task that names its existing `README.md` with a change verb and holds no
/// creation grant. Preparation records it as a required existing target.
fn required_existing_task(workspace: &Workspace, title: &str) -> (Task, Value) {
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: format!("Update {EXISTING} to cover {title}."),
            acceptance_criteria: vec!["The README documents the change.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            context_files: owned(&[EXISTING]),
            ..Default::default()
        })
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    assert_eq!(
        prepared["tasks"][0]["required_existing_selectors"],
        json!([EXISTING]),
        "{prepared}"
    );
    (task, prepared)
}

/// Without a creation grant, a required existing target does not stop an
/// assessment from settling as `verified_no_diff` (a no-diff deliverable or
/// already-landed work) or `host_operational`; a `selectors` assessment that
/// omits the target stays refused.
fn required_existing_target_allows_no_diff_and_host_operational_dispositions() {
    // A no-diff deliverable settles and is tagged `no-diff-expected`.
    let workspace = Workspace::new();
    let (task, prepared) = required_existing_task(&workspace, "deliverable");
    let applied =
        workspace.apply_disposition(&prepared, &task.context_files, &[], "verified_no_diff");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    let stored = workspace.runtime.get_task(&task.id).unwrap();
    assert!(stored.context_files.is_empty(), "{stored:?}");
    assert!(
        stored.tags.iter().any(|tag| tag == "no-diff-expected"),
        "{stored:?}"
    );

    // An already-landed finding settles too, leaving no selectors behind.
    let workspace = Workspace::new();
    let (task, prepared) = required_existing_task(&workspace, "landed");
    let landed = json!({"already_landed": {
        "evidence": "The README already documents the change at the pinned source.",
    }});
    let applied = workspace.apply_with(
        &prepared,
        &task.context_files,
        &[],
        "verified_no_diff",
        landed,
    );
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(applied["tasks"][0]["disposition"], "verified_no_diff");
    let stored = workspace.runtime.get_task(&task.id).unwrap();
    assert!(stored.context_files.is_empty(), "{stored:?}");

    // A host-operational task settles without keeping the target.
    let workspace = Workspace::new();
    let (task, prepared) = required_existing_task(&workspace, "operational");
    let applied =
        workspace.apply_disposition(&prepared, &task.context_files, &[], "host_operational");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(applied["tasks"][0]["disposition"], "host_operational");

    // Omitting the required target from a `selectors` assessment is refused.
    let workspace = Workspace::new();
    workspace.commit("docs/extra.md", "# extra\n");
    let extra = "file:docs/extra.md";
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "two targets".into(),
            description: format!("Update {EXISTING} and edit {extra}."),
            acceptance_criteria: vec!["Both documents change.".into()],
            plan: "Inspect both.".into(),
            status: Some(TaskStatus::Proposed),
            context_files: owned(&[EXISTING, extra]),
            ..Default::default()
        })
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let omitted = workspace.apply_one(&prepared, &task.context_files, &[EXISTING]);
    assert_eq!(outcome(&omitted)["outcome"], "invalid", "{omitted}");
    let error = outcome(&omitted)["error"].as_str().unwrap();
    assert!(
        error.contains(extra) && !error.contains(EXISTING),
        "{error}"
    );
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    declared_creation_targets_survive_prepare_apply_restart_and_replay_scenarios,
    creation_landed_uses_nested_evidence_when_top_level_is_blank,
    creation_landed_requires_present_targets_and_complete_pinned_covering_proof,
    declared_creation_targets_survive_prepare_apply_restart_and_replay,
    undeclared_missing_selectors_stay_refused,
    legacy_missing_target_is_reported_and_reauthorized_through_task_update,
    creation_grant_changed_after_preparation_supersedes_the_write,
    revoke_and_same_scope_reauthorization_supersedes_an_older_preparation,
    benign_comment_and_artifact_writes_preserve_preparation,
    newer_creation_grant_reaches_partition_and_ungranted_missing_path_stays_gap,
    migration_footprint_keeps_existing_targets_beside_granted_creations,
    migration_contract_preserves_narrowing_read_only_omissions_and_creation_only_work,
    required_existing_target_allows_no_diff_and_host_operational_dispositions
);
