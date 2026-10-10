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
use orbit_types::task::{CONTEXT_CREATION_AUTHORIZED_EVENT, TaskRelation, TaskRelationType};
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
            "[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.sol]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.grok]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\nsystem_crew = \"fixture\"\n",
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
            .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
            .with_automation_machine_identity(Some("fixture-machine".into()));
        Self {
            _root: root,
            runtime,
            repo,
        }
    }

    /// Run git in the fixture repository and return its trimmed stdout.
    fn git(&self, args: &[&str]) -> String {
        let mut command = std::process::Command::new("git");
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let output = command.args(args).current_dir(&self.repo).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// Commit a README edit on the current branch and return its SHA.
    fn commit(&self, message: &str) -> String {
        self.commit_file("README.md", &format!("{message}\n"), message)
    }

    /// Commit one file on the current branch and return its SHA.
    fn commit_file(&self, path: &str, contents: &str, message: &str) -> String {
        std::fs::write(self.repo.join(path), contents).unwrap();
        self.git(&["add", path]);
        self.git(&["commit", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Commit one file on a side branch and return to `main`.
    fn branch_commit_file(
        &self,
        branch: &str,
        path: &str,
        contents: &str,
        message: &str,
    ) -> String {
        self.git(&["checkout", "-b", branch]);
        let sha = self.commit_file(path, contents, message);
        self.git(&["checkout", "main"]);
        sha
    }

    /// Merge `branch` into the current branch and return the merge SHA.
    fn merge(&self, branch: &str, message: &str) -> String {
        self.git(&["merge", "--no-ff", branch, "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// A commit on a side branch, so it exists but is not on `main`.
    fn side_commit(&self) -> String {
        self.git(&["checkout", "-b", "side"]);
        let sha = self.commit("side change");
        self.git(&["checkout", "main"]);
        sha
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

    /// A qualifying proposed review finding with its own `description` and
    /// `relations`.
    fn finding(
        &self,
        title: &str,
        description: String,
        context: &[&str],
        relations: Vec<TaskRelation>,
    ) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description,
                acceptance_criteria: vec!["The change is in place.".into()],
                plan: "Edit README.md.".into(),
                status: Some(TaskStatus::Proposed),
                tags: vec!["delivery-code-review".into()],
                context_files: context.iter().map(|file| file.to_string()).collect(),
                complexity: TaskComplexity::Low,
                relations,
                ..Default::default()
            })
            .unwrap()
    }

    fn task_with_crew(
        &self,
        title: &str,
        tags: &[&str],
        context: &[&str],
        complexity: TaskComplexity,
        crew: Option<&str>,
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
                crew: crew.map(ToString::to_string),
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

    fn edit_description(&self, task: &Task) {
        self.edit(
            task,
            TaskUpdateParams {
                description: Some(format!("{} The description changed.", task.description)),
                ..Default::default()
            },
        );
    }

    fn edit_plan(&self, task: &Task) {
        self.edit(
            task,
            TaskUpdateParams {
                plan: Some(format!("{} Verify the updated plan.", task.plan)),
                ..Default::default()
            },
        );
    }

    fn edit_context(&self, task: &Task) {
        self.edit(
            task,
            TaskUpdateParams {
                context_files: Some(vec![NEW_FILE.to_string()]),
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

    // Each public document/scope edit must release a fresh hold even though
    // the task's creation grant is re-sealed by the same write.
    let hold_creation_again = || {
        let selection = workspace.select(&drain);
        assert!(selected(&selection, &creating), "{selection}");
        let task_ids = json!([creating.id.clone()]);
        let pilot = workspace.pilot_child(&drain, &task_ids);
        let applied = workspace
            .pilot(&drain, &pilot, vec![duplicate(&creating)])
            .unwrap();
        assert_eq!(applied["status"], "succeeded", "{applied}");
        let held = workspace.select(&drain);
        assert!(!selected(&held, &creating), "{held}");
        assert_eq!(held_reason(&held, &creating), Some("duplicate"));
    };

    hold_creation_again();
    workspace.edit_description(&creating);
    assert!(selected(&workspace.select(&drain), &creating));

    hold_creation_again();
    workspace.edit_plan(&creating);
    assert!(selected(&workspace.select(&drain), &creating));

    hold_creation_again();
    workspace.edit_context(&creating);
    assert!(selected(&workspace.select(&drain), &creating));
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

    // A failed pilot remains unresolved until each real document/scope edit
    // records a semantic history entry beyond the seal-only grant row.
    let fail_creation_pilot = || {
        let selection = workspace.select(&drain);
        assert!(selected(&selection, &creating), "{selection}");
        let pilot = workspace.pilot_child(&drain, &json!([creating.id.clone()]));
        workspace.pilot_failed(&pilot);
        let unresolved = workspace.select(&drain);
        assert!(!selected(&unresolved, &creating), "{unresolved}");
        assert_eq!(
            held_reason(&unresolved, &creating),
            Some("pilot_unresolved")
        );
    };

    fail_creation_pilot();
    workspace.edit_description(&creating);
    assert!(selected(&workspace.select(&drain), &creating));

    fail_creation_pilot();
    workspace.edit_plan(&creating);
    assert!(selected(&workspace.select(&drain), &creating));

    fail_creation_pilot();
    workspace.edit_context(&creating);
    assert!(selected(&workspace.select(&drain), &creating));
}

fn selected(selection: &Value, task: &Task) -> bool {
    selection["task_ids"]
        .as_array()
        .is_some_and(|task_ids| task_ids.contains(&json!(task.id)))
}

#[test]
fn proposed_candidates_of_mixed_crews_are_selected_homogeneously_and_deferred() {
    if !super::dispatch_admission::isolated(
        "drain_approval::proposed_candidates_of_mixed_crews_are_selected_homogeneously_and_deferred",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running(
        "workspace_auto_pipeline",
        json!({"approve_proposed": true, "for_seconds": 3600}),
    );
    let task_a1 = workspace.task_with_crew(
        "task_a1",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
        Some("sol"),
    );
    let task_b = workspace.task_with_crew(
        "task_b",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
        Some("grok"),
    );
    let task_a2 = workspace.task_with_crew(
        "task_a2",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
        Some("sol"),
    );

    // Pass 1: selection returns only the A tasks, in order, and reports the B task as deferred.
    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([task_a1.id, task_a2.id]));
    assert_eq!(selection["candidate_count"], 2);
    assert_eq!(selection["deferred_candidates"], 1);

    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let applied = workspace
        .pilot(
            &drain,
            &pilot,
            vec![assessment(&task_a1), assessment(&task_a2)],
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(workspace.status(&task_a1), TaskStatus::Backlog);
    assert_eq!(workspace.status(&task_a2), TaskStatus::Backlog);
    assert_eq!(workspace.status(&task_b), TaskStatus::Proposed);

    // Pass 2: following pass selects the B task.
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([task_b.id]));
    assert_eq!(next["candidate_count"], 1);
    assert_eq!(next["deferred_candidates"], 0);
}

#[test]
fn candidates_with_no_crew_are_never_bundled_with_crewed_candidates() {
    if !super::dispatch_admission::isolated(
        "drain_approval::candidates_with_no_crew_are_never_bundled_with_crewed_candidates",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let drain = workspace.running(
        "workspace_auto_pipeline",
        json!({"approve_proposed": true, "for_seconds": 3600}),
    );
    let task_uncrewed_1 = workspace.task_with_crew(
        "uncrewed_1",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
        None,
    );
    let task_crewed = workspace.task_with_crew(
        "crewed",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
        Some("sol"),
    );
    let task_uncrewed_2 = workspace.task_with_crew(
        "uncrewed_2",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
        None,
    );

    // Pass 1: only uncrewed tasks are selected in order; crewed task is deferred.
    let selection = workspace.select(&drain);
    assert_eq!(
        selection["task_ids"],
        json!([task_uncrewed_1.id, task_uncrewed_2.id])
    );
    assert_eq!(selection["candidate_count"], 2);
    assert_eq!(selection["deferred_candidates"], 1);

    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let applied = workspace
        .pilot(
            &drain,
            &pilot,
            vec![assessment(&task_uncrewed_1), assessment(&task_uncrewed_2)],
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(workspace.status(&task_uncrewed_1), TaskStatus::Backlog);
    assert_eq!(workspace.status(&task_uncrewed_2), TaskStatus::Backlog);
    assert_eq!(workspace.status(&task_crewed), TaskStatus::Proposed);

    // Pass 2: following pass selects the crewed task alone.
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([task_crewed.id]));
    assert_eq!(next["candidate_count"], 1);
    assert_eq!(next["deferred_candidates"], 0);
}

/// A `verified_no_diff` assessment whose evidence is `evidence`.
fn verified_no_diff(task: &Task, evidence: &str) -> Value {
    let mut verified = assessment(task);
    verified["context_files_after"] = json!([]);
    verified["disposition"] = json!("verified_no_diff");
    verified["evidence"] = json!(evidence);
    verified["assessment_rationale"] = json!("The fix is already on the base branch.");
    verified
}

#[test]
fn verified_no_diff_merge_proof_uses_only_the_first_parent_change() {
    if !super::dispatch_admission::isolated(
        "drain_approval::verified_no_diff_merge_proof_uses_only_the_first_parent_change",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.branch_commit_file(
        "unrelated",
        "side.txt",
        "unrelated change\n",
        "unrelated branch change",
    );
    let culprit = workspace.commit("the culprit");
    let unrelated_merge = workspace.merge("unrelated", "merge unrelated branch");
    workspace.branch_commit_file("touching", "README.md", "later fix\n", "later README fix");
    let covering_merge = workspace.merge("touching", "merge README fix");
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let finding = |title: &str| {
        workspace.finding(
            title,
            format!("Introduced by commit {culprit}."),
            &["file:README.md"],
            Vec::new(),
        )
    };
    let unrelated = finding("unrelated merge does not cover the finding");
    let covered = finding("first-parent change covers the finding");
    let pilot = workspace.running("task_pilot_pipeline", json!({}));
    let applied = workspace
        .apply(
            &pilot,
            vec![
                verified_no_diff(
                    &unrelated,
                    &format!("Commit {unrelated_merge} already fixed the README."),
                ),
                verified_no_diff(
                    &covered,
                    &format!("Commit {covering_merge} already fixed the README."),
                ),
            ],
            json!({}),
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");

    let selection = workspace.select(&drain);
    assert_eq!(selection["closed"], json!([covered.id]));
    assert_eq!(workspace.status(&unrelated), TaskStatus::Proposed);
    assert_eq!(
        held_reason(&selection, &unrelated),
        Some("pilot_verified_no_diff"),
        "a merge's second-parent diff must not make it cover the finding: {selection}"
    );
    assert_eq!(workspace.status(&covered), TaskStatus::Archived);
}

fn held_detail<'a>(selection: &'a Value, task: &Task) -> Option<&'a str> {
    selection["held"]
        .as_array()?
        .iter()
        .find(|held| held["task_id"] == task.id.as_str())?["detail"]
        .as_str()
}

#[test]
fn an_auto_minted_verified_no_diff_is_archived_only_with_ancestor_proof() {
    if !super::dispatch_admission::isolated(
        "drain_approval::an_auto_minted_verified_no_diff_is_archived_only_with_ancestor_proof",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let landed = workspace.commit("the fix");
    let side = workspace.side_commit();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let sweep_tags = ["ci-failure-sweep", "ci-failure:fixture-key"];
    let covered = workspace.task(
        "covered",
        &sweep_tags,
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let off_base = workspace.task(
        "off base",
        &sweep_tags,
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let unknown = workspace.task(
        "unknown sha",
        &["auto-task:delivery-code-review"],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let uncited = workspace.task(
        "uncited",
        &["delivery-code-review"],
        &["file:README.md"],
        TaskComplexity::Low,
    );

    // Piloted outside the drain, as the CI sweep or an operator does.
    let pilot = workspace.running("task_pilot_pipeline", json!({}));
    let applied = workspace
        .apply(
            &pilot,
            vec![
                verified_no_diff(
                    &covered,
                    &format!("Commit {landed} already fixed the README."),
                ),
                verified_no_diff(
                    &off_base,
                    &format!("Commit {side} already fixed the README."),
                ),
                verified_no_diff(
                    &unknown,
                    &format!("Run 18234567890 passed after {landed} and 0badc0ffee1."),
                ),
                verified_no_diff(
                    &uncited,
                    "The failing test does not exist at this revision.",
                ),
            ],
            json!({}),
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");

    let selection = workspace.select(&drain);
    assert_eq!(selection["closed"], json!([covered.id]), "{selection}");
    assert_eq!(
        selection["task_ids"],
        json!([]),
        "no verified-no-diff task is piloted again"
    );
    assert_eq!(workspace.status(&covered), TaskStatus::Archived);
    let comment = workspace
        .runtime
        .get_task_comments(&covered.id)
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        comment.message.contains(&landed) && comment.message.contains("operation_id="),
        "the archive comment names the commit and the assessment: {comment:?}"
    );

    for (task, cited) in [(&off_base, side.as_str()), (&unknown, "0badc0ffee1")] {
        assert_eq!(workspace.status(task), TaskStatus::Proposed);
        assert_eq!(
            held_reason(&selection, task),
            Some("pilot_verified_no_diff")
        );
        assert!(
            held_detail(&selection, task).is_some_and(|detail| detail.contains(cited)),
            "{selection}"
        );
    }
    assert_eq!(workspace.status(&uncited), TaskStatus::Proposed);
    assert_eq!(
        held_reason(&selection, &uncited),
        Some("pilot_verified_no_diff")
    );
    assert!(
        held_detail(&selection, &uncited).is_some_and(|detail| detail.contains("no commit cited")),
        "{selection}"
    );

    let report = workspace
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_approvals
        .unwrap();
    assert_eq!(report.closed_total, 1);
    assert_eq!(report.closed, vec![covered.id.clone()]);
    assert_eq!(report.held_by_reason["pilot_verified_no_diff"], 3);
}

#[test]
fn a_verified_no_diff_citing_only_the_findings_own_commits_is_held() {
    if !super::dispatch_admission::isolated(
        "drain_approval::a_verified_no_diff_citing_only_the_findings_own_commits_is_held",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let older = workspace.commit("older change");
    let culprit = workspace.commit("the culprit");
    let fix = workspace.commit("the fix");
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let named = |title: &str| {
        format!(
            "{title}: introduced by {} on the base branch.",
            &culprit[..9]
        )
    };
    // The relation's target has no observed delivery, so it adds no commit;
    // the description is the culprit signal.
    let regression = TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: workspace
            .task(
                "culprit task",
                &[],
                &["file:README.md"],
                TaskComplexity::Low,
            )
            .id,
    };
    let own_culprit = workspace.finding(
        "own culprit",
        named("own culprit"),
        &["file:README.md"],
        vec![regression.clone()],
    );
    let fixed = workspace.finding(
        "fixed after culprit",
        named("fixed after culprit"),
        &["file:README.md"],
        vec![regression.clone()],
    );
    let before_culprit = workspace.finding(
        "cites older commit",
        named("cites older commit"),
        &["file:README.md"],
        vec![regression],
    );
    let other_path = workspace.finding(
        "other path",
        "A finding that names no commit.".into(),
        &["file:src/other.rs"],
        Vec::new(),
    );
    let related = workspace.finding(
        "related change",
        "A finding that names no commit.".into(),
        &["file:README.md"],
        Vec::new(),
    );

    let pilot = workspace.running("task_pilot_pipeline", json!({}));
    let applied = workspace
        .apply(
            &pilot,
            vec![
                // Quoting the culprit the finding names is not a fix.
                verified_no_diff(&own_culprit, &format!("Commit {culprit} is the culprit.")),
                verified_no_diff(
                    &fixed,
                    &format!("Commit {culprit} broke it and {fix} repaired the README."),
                ),
                verified_no_diff(
                    &before_culprit,
                    &format!("Commit {older} touched the README."),
                ),
                verified_no_diff(&other_path, &format!("Commit {fix} repaired the README.")),
                verified_no_diff(&related, &format!("Commit {fix} repaired the README.")),
            ],
            json!({}),
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");

    let selection = workspace.select(&drain);
    let mut closed = selection["closed"].as_array().unwrap().clone();
    closed.sort_by_key(|id| id.as_str().unwrap().to_string());
    let mut expected = vec![json!(fixed.id), json!(related.id)];
    expected.sort_by_key(|id| id.as_str().unwrap().to_string());
    assert_eq!(closed, expected, "{selection}");
    for held in [&own_culprit, &before_culprit, &other_path] {
        assert_eq!(
            workspace.status(held),
            TaskStatus::Proposed,
            "{}",
            held.title
        );
        assert_eq!(
            held_reason(&selection, held),
            Some("pilot_verified_no_diff"),
            "{selection}"
        );
    }
    for archived in [&fixed, &related] {
        assert_eq!(workspace.status(archived), TaskStatus::Archived);
    }
    let comment = workspace
        .runtime
        .get_task_comments(&fixed.id)
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        comment.message.contains("Covering commit(s)")
            && comment.message.contains(&format!(": {fix}.")),
        "only the later commit is named as covering: {comment:?}"
    );
}

#[test]
fn a_culprit_named_by_an_all_digit_abbreviation_still_does_not_count() {
    if !super::dispatch_admission::isolated(
        "drain_approval::a_culprit_named_by_an_all_digit_abbreviation_still_does_not_count",
    ) {
        return;
    }
    let workspace = Workspace::new();
    // About 4% of 7-character abbreviations are all digits: recommit the
    // culprit until its abbreviation is one.
    let culprit = (0..1000)
        .map(|attempt| workspace.commit(&format!("the culprit {attempt}")))
        .find(|sha| sha[..7].bytes().all(|byte| byte.is_ascii_digit()))
        .expect("an all-digit abbreviation within 1000 commits");
    let fix = workspace.commit("the fix");
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let description = format!("Introduced by {} on the base branch.", &culprit[..7]);
    let own_culprit = workspace.finding(
        "own culprit",
        description.clone(),
        &["file:README.md"],
        Vec::new(),
    );
    let fixed = workspace.finding(
        "fixed after culprit",
        description,
        &["file:README.md"],
        Vec::new(),
    );

    let pilot = workspace.running("task_pilot_pipeline", json!({}));
    let applied = workspace
        .apply(
            &pilot,
            vec![
                verified_no_diff(&own_culprit, &format!("Commit {culprit} is the culprit.")),
                verified_no_diff(&fixed, &format!("Commit {fix} repaired the README.")),
            ],
            json!({}),
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");

    let selection = workspace.select(&drain);
    assert_eq!(selection["closed"], json!([fixed.id]), "{selection}");
    assert_eq!(workspace.status(&own_culprit), TaskStatus::Proposed);
    assert_eq!(
        held_reason(&selection, &own_culprit),
        Some("pilot_verified_no_diff"),
        "{selection}"
    );
}

#[test]
fn an_orchestrator_filed_verified_no_diff_is_held_with_its_evidence_and_never_closed() {
    if !super::dispatch_admission::isolated(
        "drain_approval::an_orchestrator_filed_verified_no_diff_is_held_with_its_evidence_and_never_closed",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let landed = workspace.commit("the fix");
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let filed = workspace.task(
        "filed",
        &["drain-robustness"],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let evidence = format!("Commit {landed} already carries the README change.");

    // The drain pilots it and holds it under the real reason.
    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([filed.id]));
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let applied = workspace
        .pilot(&drain, &pilot, vec![verified_no_diff(&filed, &evidence)])
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    let decision = &applied["drain_approval"][0];
    assert_eq!(
        decision["classification"], "pilot_verified_no_diff",
        "{applied}"
    );
    assert_eq!(decision["evidence"]["cited_commits"], json!([landed]));
    assert_eq!(decision["closed"], false, "{applied}");

    let next = workspace.select(&drain);
    assert_eq!(workspace.status(&filed), TaskStatus::Proposed);
    assert_eq!(next["closed"], json!([]));
    assert_eq!(held_reason(&next, &filed), Some("pilot_verified_no_diff"));
    assert!(
        held_detail(&next, &filed).is_some_and(|detail| detail.contains(&evidence)),
        "{next}"
    );

    // `orbit run show` renders the drain's report; readiness reads the same.
    let report = workspace
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_approvals
        .unwrap();
    let held = report
        .held
        .iter()
        .find(|held| held.task_id == filed.id)
        .unwrap();
    assert_eq!(held.reason.as_deref(), Some("pilot_verified_no_diff"));
    assert!(
        held.detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&landed))
    );
    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(
        held_reason(&readiness["approvals"], &filed),
        Some("pilot_verified_no_diff"),
        "{readiness}"
    );
    assert!(
        held_detail(&readiness["approvals"], &filed)
            .is_some_and(|detail| detail.contains(&evidence)),
        "{readiness}"
    );
    assert_eq!(workspace.status(&filed), TaskStatus::Proposed);
}

#[test]
fn the_ci_sweep_closes_a_verified_no_diff_by_the_same_proof() {
    if !super::dispatch_admission::isolated(
        "drain_approval::the_ci_sweep_closes_a_verified_no_diff_by_the_same_proof",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let landed = workspace.commit("the fix");
    let sweep_apply = |task: &Task, evidence: &str| {
        let sweep = workspace.running("ci_failure_sweep_pipeline", json!({}));
        workspace
            .apply(
                &sweep,
                vec![verified_no_diff(task, evidence)],
                json!({"ci_sweep_filing": {
                    "task_id": task.id, "failure_key": "fixture-key", "tested_commit": "0123abcd",
                    "workflow": "ci", "job": "test", "step": "cargo test",
                    "run_urls": ["https://github.com/example/repo/actions/runs/1"],
                }}),
            )
            .unwrap()
    };
    let tags = ["ci-failure-sweep", "ci-failure:fixture-key"];

    let covered = workspace.task("covered", &tags, &["file:README.md"], TaskComplexity::Low);
    let applied = sweep_apply(&covered, &format!("Fixed by {landed}."));
    let decision = &applied["ci_sweep_admission"][0];
    assert_eq!(
        decision["classification"], "pilot_verified_no_diff",
        "{applied}"
    );
    assert_eq!(decision["closed"], true, "{applied}");
    assert_eq!(decision["covering_commits"], json!([landed]));
    assert_eq!(workspace.status(&covered), TaskStatus::Archived);

    let uncited = workspace.task("uncited", &tags, &["file:README.md"], TaskComplexity::Low);
    let applied = sweep_apply(
        &uncited,
        "The failing test does not exist at this revision.",
    );
    let decision = &applied["ci_sweep_admission"][0];
    assert_eq!(
        decision["classification"], "pilot_verified_no_diff",
        "{applied}"
    );
    assert_eq!(decision["closed"], false, "{applied}");
    assert_eq!(workspace.status(&uncited), TaskStatus::Proposed);
}
