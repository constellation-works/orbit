//! `orbit run auto --approve-proposed` [ORB-14117]: one drain pass selects
//! qualifying proposed tasks, applies their pilot results under the drain's
//! verified authority, records the approvals, and admits the approved work.
//! The deterministic steps run in an isolated child against real stores; the
//! agent pilot's output is supplied as fixture data.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, Task, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::CONTEXT_CREATION_AUTHORIZED_EVENT;
use orbit_types::workflow::{ChildDispatch, JobRunState, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Workspace {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            repo.join(".orbit/config.toml"),
            "[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\nsystem_crew = \"fixture\"\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let mut command = std::process::Command::new("git");
            orbit_common::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            let output = command.args(args).current_dir(&repo).output().unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Orbit Test"]);
        git(&["config", "user.email", "orbit-test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "seed"]);
        let activities = global.join("resources/activities");
        std::fs::create_dir_all(&activities).unwrap();
        for name in [
            "prepare_task_pilot",
            "task_pilot",
            "apply_task_pilot_results",
            "select_proposed_approvals",
            "record_proposed_approvals",
            "classify_workspace_auto_tasks",
        ] {
            let file = format!("{name}.yaml");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/activities")
                    .join(&file),
                activities.join(file),
            )
            .unwrap();
        }
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some("fixture-machine".into()));
        Self {
            _root: root,
            runtime,
            repo,
        }
    }

    fn task(
        &self,
        title: &str,
        tags: &[&str],
        context: &[&str],
        complexity: TaskComplexity,
    ) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Ship {title}."),
                acceptance_criteria: vec!["The change is in place.".into()],
                plan: "Edit README.md.".into(),
                status: Some(TaskStatus::Proposed),
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                context_files: context.iter().map(|file| file.to_string()).collect(),
                complexity,
                ..Default::default()
            })
            .unwrap()
    }

    /// A qualifying proposed task that will also create `file:src/new.rs`,
    /// declared the way an operator surface declares a creation target.
    fn creation_task(&self, title: &str) -> Task {
        let context_files = vec!["file:README.md".to_string(), NEW_FILE.to_string()];
        let context_creation = self
            .runtime
            .authorize_missing_context(&context_files)
            .unwrap();
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Ship {title}."),
                acceptance_criteria: vec!["The change is in place.".into()],
                plan: "Edit README.md and add src/new.rs.".into(),
                status: Some(TaskStatus::Proposed),
                context_files,
                complexity: TaskComplexity::Low,
                context_creation,
                ..Default::default()
            })
            .unwrap()
    }

    fn edit(&self, task: &Task, params: TaskUpdateParams) {
        self.runtime
            .update_task_with_identity(&task.id, params, Some("codex".into()), None)
            .unwrap();
    }

    /// A comment changes nothing the pilot judged; on a task holding a
    /// creation grant it still re-seals the grant in history.
    fn comment(&self, task: &Task) {
        self.edit(
            task,
            TaskUpdateParams {
                comment: Some("Still relevant.".into()),
                ..Default::default()
            },
        );
    }

    /// A real operator edit the drain must treat as a change.
    fn rename(&self, task: &Task) {
        self.edit(
            task,
            TaskUpdateParams {
                title: Some(format!("{} (revised)", task.title)),
                ..Default::default()
            },
        );
    }

    fn last_event(&self, task: &Task) -> String {
        self.runtime
            .get_task_history(&task.id)
            .unwrap()
            .pop()
            .unwrap()
            .event
    }

    /// Finish the pilot child `pilot` without applying any result, as a
    /// failed or stale pilot does.
    fn pilot_failed(&self, pilot: &str) {
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        jobs.finalize_job_run(pilot, JobRunState::Failed, Utc::now(), None)
            .unwrap();
    }

    fn status(&self, task: &Task) -> TaskStatus {
        self.runtime.get_task(&task.id).unwrap().status
    }

    fn action(&self, action: &str, input: Value) -> Result<Value, String> {
        self.runtime
            .run_deterministic(action, &json!({}), &input, ToolContext::default())
            .map_err(|error| error.to_string())
    }

    /// A running run of `job`, as the drain and its pilot child are.
    fn running(&self, job: &str, input: Value) -> String {
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run(job, 1, Utc::now(), Some(input), None)
            .unwrap();
        self.runtime
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
            )
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        run.run_id
    }

    /// The pilot child the drain's `approve_proposed` step dispatches.
    fn pilot_child(&self, drain: &str, task_ids: &Value) -> String {
        let pilot = self.running("task_pilot_pipeline", json!({ "task_ids": task_ids }));
        let mut state = self.runtime.read_run_state(drain).unwrap().unwrap();
        state.record_child_dispatch(ChildDispatch::submitted(
            pilot.clone(),
            "task_pilot_pipeline".into(),
            "invoke_and_wait".into(),
            true,
            false,
            Utc::now(),
        ));
        self.runtime.write_run_state(drain, &state).unwrap();
        pilot
    }

    fn select(&self, drain: &str) -> Value {
        self.action(
            "select_proposed_approvals",
            json!({ "approve_proposed": true, "run_id": drain }),
        )
        .unwrap()
    }

    /// Prepare and apply `assessments` the way the pilot child's steps do,
    /// with the authority record the drain hands it.
    fn pilot(&self, drain: &str, pilot: &str, assessments: Vec<Value>) -> Result<Value, String> {
        self.apply(
            pilot,
            assessments,
            json!({"drain_promotion": {"run_id": drain}}),
        )
    }

    /// Prepare and apply `assessments` with promotion authorized under
    /// `authority` (`drain_promotion` or `ci_sweep_filing`).
    fn apply(
        &self,
        run_id: &str,
        assessments: Vec<Value>,
        authority: Value,
    ) -> Result<Value, String> {
        let task_ids = assessments
            .iter()
            .map(|assessment| assessment["task_id"].clone())
            .collect::<Vec<_>>();
        let prepared = self.action(
            "prepare_task_pilot",
            json!({"task_ids": task_ids, "workspace_path": self.repo, "base_branch": "main"}),
        )?;
        let mut input = json!({
            "run_id": run_id,
            "workspace_path": self.repo,
            "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": task_ids, "tasks": assessments}],
            "promotion_authorized": true,
        });
        for (key, value) in authority.as_object().unwrap() {
            input[key] = value.clone();
        }
        self.action("apply_task_pilot_results", input)
    }
}

const NEW_FILE: &str = "file:src/new.rs";

/// A clean pilot assessment; callers add findings.
fn assessment(task: &Task) -> Value {
    json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md holds the change.",
        "validation_approach": "Inspect README.md.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    })
}

fn held_reason<'a>(selection: &'a Value, task: &Task) -> Option<&'a str> {
    selection["held"]
        .as_array()?
        .iter()
        .find(|held| held["task_id"] == task.id.as_str())?["reason"]
        .as_str()
}

#[test]
fn tasks_filed_mid_window_are_approved_by_tag_or_fields_and_admitted_the_same_pass() {
    if !super::dispatch_admission::isolated(
        "drain_approval::tasks_filed_mid_window_are_approved_by_tag_or_fields_and_admitted_the_same_pass",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running(
        "workspace_auto_pipeline",
        json!({"approve_proposed": true, "for_seconds": 3600}),
    );
    // Filed after the window opened.
    let tagged = workspace.task(
        "tagged",
        &["no-diff-expected"],
        &[],
        TaskComplexity::Unassessed,
    );
    let scoped = workspace.task("scoped", &[], &["file:README.md"], TaskComplexity::Low);

    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([tagged.id, scoped.id]));
    assert_eq!(selection["drain_promotion"], json!({"run_id": drain}));

    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let mut no_diff = assessment(&tagged);
    no_diff["context_files_after"] = json!([]);
    no_diff["disposition"] = json!("verified_no_diff");
    no_diff["evidence"] = json!("The result is a filed report outside the repository.");
    let applied = workspace
        .pilot(&drain, &pilot, vec![no_diff, assessment(&scoped)])
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    let decisions = applied["drain_approval"].as_array().unwrap();
    assert!(
        decisions
            .iter()
            .all(|decision| decision["decision"] == "promote" && decision["approved"] == true),
        "{applied}"
    );

    for task in [&tagged, &scoped] {
        assert_eq!(workspace.status(task), TaskStatus::Backlog);
        let approval = workspace
            .runtime
            .get_task_history(&task.id)
            .unwrap()
            .into_iter()
            .find(|entry| entry.event == "proposal_approved")
            .expect("approve transition");
        assert!(
            approval
                .note
                .as_deref()
                .unwrap_or_default()
                .contains(&drain),
            "the approval names the drain: {approval:?}"
        );
    }

    let recorded = workspace
        .action(
            "record_proposed_approvals",
            json!({"run_id": drain, "task_ids": selection["task_ids"]}),
        )
        .unwrap();
    assert_eq!(recorded["approved"], json!([tagged.id, scoped.id]));
    let report = workspace
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_approvals
        .unwrap();
    assert_eq!(report.approved_total, 2);

    let admitted = workspace
        .action(
            "classify_workspace_auto_tasks",
            json!({"run_id": drain, "max_active_leaf_runs": 4}),
        )
        .unwrap();
    assert!(
        admitted["loose_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(scoped.id)),
        "the approved task is admitted in the same pass: {admitted}"
    );
}

#[test]
fn unqualified_and_pilot_held_tasks_stay_proposed_with_their_reasons() {
    if !super::dispatch_admission::isolated(
        "drain_approval::unqualified_and_pilot_held_tasks_stay_proposed_with_their_reasons",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let unassessed = workspace.task(
        "unassessed",
        &[],
        &["file:README.md"],
        TaskComplexity::Unassessed,
    );
    let bare = workspace.task("bare", &[], &[], TaskComplexity::Low);
    let original = workspace.task("original", &[], &["file:README.md"], TaskComplexity::Low);
    let duplicate = workspace.task("duplicate", &[], &["file:README.md"], TaskComplexity::Low);

    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([original.id, duplicate.id]));
    assert_eq!(
        held_reason(&selection, &unassessed),
        Some("unassessed_complexity")
    );
    assert_eq!(
        held_reason(&selection, &bare),
        Some("missing_context_files")
    );

    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let mut duplicated = assessment(&duplicate);
    duplicated["duplicate_of"] =
        json!({"task_id": original.id, "evidence": "Both tasks make the same README edit."});
    let applied = workspace
        .pilot(&drain, &pilot, vec![assessment(&original), duplicated])
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(workspace.status(&original), TaskStatus::Backlog);
    for task in [&unassessed, &bare, &duplicate] {
        assert_eq!(workspace.status(task), TaskStatus::Proposed);
    }

    // The hold is not piloted again until the task changes.
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([]));
    assert_eq!(held_reason(&next, &duplicate), Some("duplicate"));
    assert_eq!(next["held_by_reason"]["duplicate"], 1);
}

#[test]
fn no_auto_approve_is_held_by_every_promotion_authority() {
    if !super::dispatch_admission::isolated(
        "drain_approval::no_auto_approve_is_held_by_every_promotion_authority",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    // Otherwise qualifying on both the tag and the fields.
    let opted_out = workspace.task(
        "opted out",
        &["no-auto-approve", "no-diff-expected"],
        &["file:README.md"],
        TaskComplexity::Low,
    );

    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([]), "it costs no pilot work");
    assert_eq!(held_reason(&selection, &opted_out), Some("no-auto-approve"));

    // The apply boundary holds it even when a drain pilots it anyway.
    let pilot = workspace.pilot_child(&drain, &json!([opted_out.id]));
    let applied = workspace
        .pilot(&drain, &pilot, vec![assessment(&opted_out)])
        .unwrap();
    assert_eq!(
        applied["drain_approval"][0]["classification"],
        "no-auto-approve"
    );
    assert_eq!(workspace.status(&opted_out), TaskStatus::Proposed);

    // And under the CI sweep's authority.
    let filed = workspace.task(
        "ci failure",
        &[
            "ci-failure-sweep",
            "ci-failure:fixture-key",
            "no-auto-approve",
        ],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let sweep = workspace.running("ci_failure_sweep_pipeline", json!({}));
    let applied = workspace
        .apply(
            &sweep,
            vec![assessment(&filed)],
            json!({"ci_sweep_filing": {
                "task_id": filed.id, "failure_key": "fixture-key", "tested_commit": "0123abcd",
                "workflow": "ci", "job": "test", "step": "cargo test",
                "run_urls": ["https://github.com/example/repo/actions/runs/1"],
            }}),
        )
        .unwrap();
    assert_eq!(
        applied["ci_sweep_admission"][0]["classification"],
        "no-auto-approve"
    );
    assert_eq!(workspace.status(&filed), TaskStatus::Proposed);
}

#[test]
fn drain_authority_is_verified_before_any_approval() {
    if !super::dispatch_admission::isolated(
        "drain_approval::drain_authority_is_verified_before_any_approval",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("scoped", &[], &["file:README.md"], TaskComplexity::Low);
    let task_ids = json!([task.id]);

    // A drain started without the flag grants nothing, even to its own child.
    let plain = workspace.running("workspace_auto_pipeline", json!({}));
    let pilot = workspace.pilot_child(&plain, &task_ids);
    let refused = workspace
        .pilot(&plain, &pilot, vec![assessment(&task)])
        .unwrap_err();
    assert!(refused.contains("approve_proposed"), "{refused}");

    // An opted-in drain grants nothing to a pilot run it did not dispatch.
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let stray = workspace.running("task_pilot_pipeline", json!({ "task_ids": task_ids }));
    let refused = workspace
        .pilot(&drain, &stray, vec![assessment(&task)])
        .unwrap_err();
    assert!(refused.contains("did not dispatch"), "{refused}");
    assert_eq!(workspace.status(&task), TaskStatus::Proposed);
}

#[test]
fn a_pilot_hold_on_a_creation_target_task_survives_until_the_task_really_changes() {
    if !super::dispatch_admission::isolated(
        "drain_approval::a_pilot_hold_on_a_creation_target_task_survives_until_the_task_really_changes",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let original = workspace.task("original", &[], &["file:README.md"], TaskComplexity::Low);
    let creating = workspace.creation_task("creating");
    let ordinary = workspace.task("ordinary", &[], &["file:README.md"], TaskComplexity::Low);

    let selection = workspace.select(&drain);
    assert_eq!(
        selection["task_ids"],
        json!([original.id, creating.id, ordinary.id])
    );
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let duplicate = |task: &Task| {
        let mut duplicated = assessment(task);
        duplicated["context_files_after"] = json!(task.context_files);
        duplicated["duplicate_of"] =
            json!({"task_id": original.id, "evidence": "Both tasks make the same README edit."});
        duplicated
    };
    let applied = workspace
        .pilot(
            &drain,
            &pilot,
            vec![
                assessment(&original),
                duplicate(&creating),
                duplicate(&ordinary),
            ],
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(workspace.status(&creating), TaskStatus::Proposed);
    assert_eq!(
        workspace.last_event(&creating),
        CONTEXT_CREATION_AUTHORIZED_EVENT,
        "the pilot write re-seals the creation grant after its hold marker"
    );

    // Held with the pilot's classification, like a task without a grant.
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([]), "{next}");
    assert_eq!(held_reason(&next, &creating), Some("duplicate"));
    assert_eq!(held_reason(&next, &ordinary), Some("duplicate"));
    assert_eq!(next["held_by_reason"]["duplicate"], 2);

    // Re-sealing the grant is not a change the pilot has not judged.
    workspace.comment(&creating);
    workspace.comment(&ordinary);
    assert_eq!(
        workspace.last_event(&creating),
        CONTEXT_CREATION_AUTHORIZED_EVENT
    );
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([]), "{next}");
    assert_eq!(held_reason(&next, &creating), Some("duplicate"));
    assert_eq!(held_reason(&next, &ordinary), Some("duplicate"));

    // A real edit releases either hold for another pilot.
    workspace.rename(&creating);
    workspace.rename(&ordinary);
    let next = workspace.select(&drain);
    assert_eq!(
        next["task_ids"],
        json!([creating.id, ordinary.id]),
        "{next}"
    );
}

#[test]
fn an_unresolved_pilot_of_a_creation_target_task_is_retried_only_after_a_real_change() {
    if !super::dispatch_admission::isolated(
        "drain_approval::an_unresolved_pilot_of_a_creation_target_task_is_retried_only_after_a_real_change",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let creating = workspace.creation_task("creating");
    let ordinary = workspace.task("ordinary", &[], &["file:README.md"], TaskComplexity::Low);

    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([creating.id, ordinary.id]));
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    workspace.pilot_failed(&pilot);

    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([]), "{next}");
    assert_eq!(held_reason(&next, &creating), Some("pilot_unresolved"));
    assert_eq!(held_reason(&next, &ordinary), Some("pilot_unresolved"));

    workspace.comment(&creating);
    workspace.comment(&ordinary);
    assert_eq!(
        workspace.last_event(&creating),
        CONTEXT_CREATION_AUTHORIZED_EVENT
    );
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([]), "{next}");
    assert_eq!(held_reason(&next, &creating), Some("pilot_unresolved"));
    assert_eq!(held_reason(&next, &ordinary), Some("pilot_unresolved"));

    workspace.rename(&creating);
    workspace.rename(&ordinary);
    let next = workspace.select(&drain);
    assert_eq!(
        next["task_ids"],
        json!([creating.id, ordinary.id]),
        "{next}"
    );
}
