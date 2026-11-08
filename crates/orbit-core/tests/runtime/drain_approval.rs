//! `orbit run auto --approve-proposed` [ORB-14117]: a drain pass selects
//! qualifying proposed tasks for a detached pilot, the pilot applies its
//! results under the drain's verified authority, and the pass that sees the
//! pilot finish records the approvals and admits the approved work. The
//! deterministic steps run in an isolated child against real stores; the
//! agent pilot's output is supplied as fixture data.
use chrono::{DateTime, Utc};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams, TaskWriteScreen};
use orbit_core::{OrbitRuntime, Task, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStepParams;
use orbit_tools::ToolContext;
use orbit_types::resource::ExecutorResource;
use orbit_types::task::{CONTEXT_CREATION_AUTHORIZED_EVENT, TaskRelation, TaskRelationType};
use orbit_types::workflow::{
    ChildDispatch, ExecutorDef, JobRunState, JobRunTrigger, JobTargetType, PipelineState,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
mod advisory;
#[cfg(unix)]
mod explicit_pilot_reuse;
mod landed_creation;
mod pilot_mutations;
#[cfg(unix)]
mod required_tools;
mod step_graph;
mod unresolved_footprint;
struct Workspace {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
}
impl Workspace {
    fn new() -> Self {
        Self::with_config("")
    }
    fn with_config(extra_config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            repo.join(".orbit/config.toml"),
            format!("[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.sol]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.grok]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"fixture\"\nsystem_crew = \"fixture\"\n{extra_config}"),
        )
        .unwrap();
        let git = |args: &[&str]| {
            let mut command = crate::git_test_support::command();
            let output = command.args(args).current_dir(&repo).output().unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Orbit Test"][..],
            &["config", "user.email", "orbit-test@example.com"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            git(args);
        }
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
            "pipeline_success_guard",
        ] {
            let file = format!("{name}.yaml");
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities");
            std::fs::copy(source.join(&file), activities.join(file)).unwrap();
        }
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let runtime = runtime
            .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
            .with_automation_machine_identity(Some("fixture-machine".into()));
        Self {
            _root: root,
            runtime,
            repo,
        }
    }
    fn git(&self, args: &[&str]) -> String {
        let mut command = crate::git_test_support::command();
        let output = command.args(args).current_dir(&self.repo).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
    fn commit(&self, message: &str) -> String {
        self.commit_file("README.md", &format!("{message}\n"), message)
    }
    fn commit_file(&self, path: &str, contents: &str, message: &str) -> String {
        std::fs::write(self.repo.join(path), contents).unwrap();
        self.git(&["add", path]);
        self.git(&["commit", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
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
    fn merge(&self, branch: &str, message: &str) -> String {
        self.git(&["merge", "--no-ff", branch, "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn side_commit(&self) -> String {
        self.git(&["checkout", "-b", "side"]);
        let sha = self.commit("side change");
        self.git(&["checkout", "main"]);
        sha
    }
    fn add(&self, params: TaskAddParams) -> Task {
        self.runtime.add_task(params).unwrap()
    }
    fn proposed_params(
        title: &str,
        tags: &[&str],
        context: &[&str],
        complexity: TaskComplexity,
    ) -> TaskAddParams {
        TaskAddParams {
            title: title.into(),
            description: format!("Ship {title}."),
            acceptance_criteria: vec!["The change is in place.".into()],
            plan: "Edit README.md.".into(),
            status: Some(TaskStatus::Proposed),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            context_files: context.iter().map(|file| file.to_string()).collect(),
            complexity,
            ..Default::default()
        }
    }
    fn task(
        &self,
        title: &str,
        tags: &[&str],
        context: &[&str],
        complexity: TaskComplexity,
    ) -> Task {
        self.add(Self::proposed_params(title, tags, context, complexity))
    }
    fn readme_task(&self, title: &str) -> Task {
        self.task(title, &[], &["file:README.md"], TaskComplexity::Low)
    }
    fn tagged_task(&self, title: &str, tags: &[&str]) -> Task {
        self.task(title, tags, &["file:README.md"], TaskComplexity::Low)
    }
    fn readme_finding(
        &self,
        title: &str,
        description: String,
        relations: Vec<TaskRelation>,
    ) -> Task {
        self.finding(title, description, &["file:README.md"], relations)
    }
    fn drain(&self) -> String {
        self.running("workspace_auto_pipeline", json!({"approve_proposed": true}))
    }
    fn jobs(&self) -> std::sync::Arc<dyn orbit_store::contracts::JobRunStoreBackend> {
        orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        )
    }
    fn crewed_task(&self, title: &str, crew: Option<&str>) -> Task {
        self.task_with_crew(title, &[], &["file:README.md"], TaskComplexity::Low, crew)
    }
    fn run_state(&self, run_id: &str) -> PipelineState {
        self.runtime.read_run_state(run_id).unwrap().unwrap()
    }
    fn drain_report(&self, drain: &str) -> orbit_types::workflow::DrainApprovalReport {
        self.run_state(drain).pull_drain.drain_approvals.unwrap()
    }
    fn history(&self, task: &Task) -> Vec<orbit_types::task::TaskHistoryEntry> {
        self.runtime.get_task_history(&task.id).unwrap()
    }
    fn approval_entry(&self, task: &Task) -> orbit_types::task::TaskHistoryEntry {
        self.history(task)
            .into_iter()
            .find(|entry| entry.event == "proposal_approved")
            .expect("approve transition")
    }
    fn last_comment(&self, task: &Task) -> orbit_types::task::TaskComment {
        self.runtime
            .get_task_comments(&task.id)
            .unwrap()
            .pop()
            .unwrap()
    }
    fn apply_ok(&self, run_id: &str, assessments: Vec<Value>, authority: Value) -> Value {
        let applied = self.apply(run_id, assessments, authority).unwrap();
        assert_eq!(applied["status"], "succeeded", "{applied}");
        applied
    }
    fn pilot_ok(&self, drain: &str, pilot: &str, assessments: Vec<Value>) -> Value {
        let applied = self.pilot(drain, pilot, assessments).unwrap();
        assert_eq!(applied["status"], "succeeded", "{applied}");
        applied
    }
    fn sweep_ok(&self, task: &Task, assessed: Value) -> Value {
        let sweep = self.running("ci_failure_sweep_pipeline", json!({}));
        self.apply_ok(
            &sweep,
            vec![assessed],
            json!({"ci_sweep_filing": {
                "task_id": task.id, "failure_key": "fixture-key", "tested_commit": "0123abcd",
                "workflow": "ci", "job": "test", "step": "cargo test",
                "run_urls": ["https://github.com/example/repo/actions/runs/1"],
            }}),
        )
    }
    fn finding(
        &self,
        title: &str,
        description: String,
        context: &[&str],
        relations: Vec<TaskRelation>,
    ) -> Task {
        let mut params = Self::proposed_params(
            title,
            &["delivery-code-review"],
            context,
            TaskComplexity::Low,
        );
        params.description = description;
        params.relations = relations;
        self.add(params)
    }
    fn task_with_crew(
        &self,
        title: &str,
        tags: &[&str],
        context: &[&str],
        complexity: TaskComplexity,
        crew: Option<&str>,
    ) -> Task {
        let mut params = Self::proposed_params(title, tags, context, complexity);
        params.crew = crew.map(ToString::to_string);
        self.add(params)
    }
    fn creation_task(&self, title: &str) -> Task {
        let mut params = Self::proposed_params(title, &[], &[], TaskComplexity::Low);
        params.plan = "Edit README.md and add src/new.rs.".into();
        params.context_files = vec!["file:README.md".to_string(), NEW_FILE.to_string()];
        self.runtime
            .add_task_screened(params, &TaskWriteScreen::unbound(true), None, None)
            .unwrap()
            .task
    }
    fn foreign_runtime(&self) -> OrbitRuntime {
        let root = self.repo.parent().unwrap().join("foreign");
        std::fs::create_dir_all(root.join(".orbit")).unwrap();
        let foreign_config = root.join(".orbit/config.toml");
        if !foreign_config.exists() {
            std::fs::copy(self.repo.join(".orbit/config.toml"), foreign_config).unwrap();
        }
        let runtime =
            OrbitRuntime::from_roots(&self.runtime.global_root(), &root.join(".orbit")).unwrap();
        runtime
            .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
            .with_automation_machine_identity(Some("fixture-machine".into()))
    }
    fn complete_foreign_task(&self, task: &Task) {
        let params = TaskUpdateParams {
            status: Some(TaskStatus::Done),
            execution_summary: Some("Foreign prerequisite completed.".into()),
            ..Default::default()
        };
        self.foreign_runtime()
            .update_task_with_identity(&task.id, params, Some("codex".into()), None)
            .unwrap();
    }
    fn foreign_task(&self, title: &str, status: TaskStatus) -> Task {
        let runtime = self.foreign_runtime();
        let mut params = Self::proposed_params(title, &[], &[], TaskComplexity::Low);
        params.status = Some(status);
        runtime.add_task(params).unwrap()
    }
    fn edit(&self, task: &Task, params: TaskUpdateParams) {
        self.runtime
            .update_task_with_identity(&task.id, params, Some("codex".into()), None)
            .unwrap();
    }
    fn edit_with(&self, task: &Task, update: impl FnOnce(&mut TaskUpdateParams)) {
        let mut params = TaskUpdateParams::default();
        update(&mut params);
        self.edit(task, params);
    }
    fn comment(&self, task: &Task) {
        self.edit_with(task, |p| p.comment = Some("Still relevant.".into()));
    }
    fn rename(&self, task: &Task) {
        self.edit_with(task, |p| {
            p.title = Some(format!("{} (revised)", task.title))
        });
    }
    fn edit_description(&self, task: &Task) {
        let value = format!("{} The description changed.", task.description);
        self.edit_with(task, |p| p.description = Some(value));
    }
    fn edit_plan(&self, task: &Task) {
        let value = format!("{} Verify the updated plan.", task.plan);
        self.edit_with(task, |p| p.plan = Some(value));
    }
    fn edit_context(&self, task: &Task) {
        self.edit_with(task, |p| p.context_files = Some(vec![NEW_FILE.to_string()]));
    }
    fn last_event(&self, task: &Task) -> String {
        self.history(task).pop().unwrap().event
    }
    fn pilot_failed(&self, pilot: &str) {
        self.finish(pilot, JobRunState::Failed);
    }
    fn finish(&self, run_id: &str, state: JobRunState) {
        let jobs = self.jobs();
        if jobs.get_job_run(run_id).unwrap().unwrap().state == JobRunState::Pending {
            jobs.mark_job_run_running(run_id, Utc::now(), std::process::id())
                .unwrap();
        }
        jobs.finalize_job_run(run_id, state, Utc::now(), None)
            .unwrap();
    }
    fn status(&self, task: &Task) -> TaskStatus {
        self.runtime.get_task(&task.id).unwrap().status
    }
    fn action(&self, action: &str, input: Value) -> Result<Value, String> {
        let mut input = input;
        if action == "apply_task_pilot_results"
            && let Some(revision) = input["prepared"]["source"]["source_revision"]
                .as_str()
                .map(str::to_string)
        {
            let tree = self.git(&["rev-parse", &format!("{revision}^{{tree}}")]);
            if let Some(results) = input["results"].as_array_mut() {
                for result in results {
                    if result.is_object()
                        && result.get("observed_revision").is_none()
                        && result.get("observed_tree").is_none()
                    {
                        result["observed_revision"] = json!(revision);
                        result["observed_tree"] = json!(tree);
                    }
                }
            }
        }
        self.runtime
            .run_deterministic(action, &json!({}), &input, ToolContext::default())
            .map_err(|error| error.to_string())
    }
    fn running(&self, job: &str, input: Value) -> String {
        let jobs = self.jobs();
        let run = jobs
            .insert_job_run(job, 1, Utc::now(), Some(input), None)
            .unwrap();
        let state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        self.runtime.write_run_state(&run.run_id, &state).unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        run.run_id
    }
    fn pilot_child(&self, drain: &str, task_ids: &Value) -> String {
        let pilot = self.running("task_pilot_pipeline", json!({ "task_ids": task_ids }));
        let mut state = self.run_state(drain);
        state.record_child_dispatch(ChildDispatch::submitted(
            pilot.clone(),
            "task_pilot_pipeline".into(),
            "invoke_detached".into(),
            false,
            false,
            Utc::now(),
        ));
        self.runtime.write_run_state(drain, &state).unwrap();
        pilot
    }
    fn select(&self, drain: &str) -> Value {
        let input = json!({ "approve_proposed": true, "run_id": drain });
        self.action("select_proposed_approvals", input).unwrap()
    }
    fn pilot(&self, drain: &str, pilot: &str, assessments: Vec<Value>) -> Result<Value, String> {
        let applied = self.apply(
            pilot,
            assessments,
            json!({"drain_promotion": {"run_id": drain}}),
        );
        let state = if applied.is_ok() {
            JobRunState::Success
        } else {
            JobRunState::Failed
        };
        self.finish(pilot, state);
        applied
    }
    fn record(&self, drain: &str) -> Value {
        self.action("record_proposed_approvals", json!({ "run_id": drain }))
            .unwrap()
    }
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
    fn set_trigger(&self, run_id: &str, trigger: JobRunTrigger) {
        let mut state = self.run_state(run_id);
        state.trigger = Some(trigger);
        self.runtime.write_run_state(run_id, &state).unwrap();
    }
    fn checkpoint_prepare(&self, run_id: &str, prepared: &Value) {
        let mut state = self.run_state(run_id);
        state.record_step(0, JobRunState::Success, Some(prepared.clone()), None);
        self.runtime.write_run_state(run_id, &state).unwrap();
    }
    fn prepare_named(&self, run_id: &str, task_ids: &[String]) -> Value {
        let input = json!({
            "task_ids": task_ids, "workspace_path": self.repo,
            "base_branch": "main", "run_id": run_id,
        });
        self.action("prepare_task_pilot", input)
            .unwrap_or_else(|error| panic!("prepare {run_id}: {error}"))
    }
    fn apply_unpromoted(&self, run_id: &str, prepared: &Value, task: &Task) -> Value {
        self.apply_unpromoted_assessment(run_id, prepared, assessment(task))
    }
    fn apply_unpromoted_assessment(
        &self,
        run_id: &str,
        prepared: &Value,
        assessed: Value,
    ) -> Value {
        let task_id = assessed["task_id"].clone();
        let mut input = json!({
            "run_id": run_id, "workspace_path": self.repo, "prepared": prepared,
            "promotion_authorized": false,
        });
        input["results"] = json!([{
            "partition_index": 0, "task_ids": [task_id], "tasks": [assessed],
        }]);
        self.action("apply_task_pilot_results", input)
            .unwrap_or_else(|error| panic!("apply without promotion: {error}"))
    }
    fn pilot_applications(&self, task: &Task) -> usize {
        self.history(task)
            .iter()
            .filter(|entry| entry.event == "task_pilot_applied")
            .count()
    }
}
const NEW_FILE: &str = "file:src/new.rs";
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
fn held_field<'a>(selection: &'a Value, task: &Task, field: &str) -> Option<&'a str> {
    selection["held"]
        .as_array()?
        .iter()
        .find(|held| held["task_id"] == task.id.as_str())?[field]
        .as_str()
}
fn held_reason<'a>(selection: &'a Value, task: &Task) -> Option<&'a str> {
    held_field(selection, task, "reason")
}
fn held_detail<'a>(selection: &'a Value, task: &Task) -> Option<&'a str> {
    held_field(selection, task, "detail")
}
fn assert_task_ids(selection: &Value, tasks: &[&Task]) {
    let ids = json!(tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>());
    assert_eq!(selection["task_ids"], ids, "{selection}");
}
fn assert_held(selection: &Value, task: &Task, reason: &str) {
    assert_eq!(held_reason(selection, task), Some(reason), "{selection}");
}
fn assert_held_detail(selection: &Value, task: &Task, needle: &str) {
    assert!(
        held_detail(selection, task).is_some_and(|d| d.contains(needle)),
        "{selection}"
    );
}
fn assert_status(ws: &Workspace, task: &Task, status: TaskStatus) {
    assert_eq!(ws.status(task), status, "task {}", task.title);
}
fn assert_classification(applied: &Value, key: &str, expected: &str) {
    assert_eq!(applied[key][0]["classification"], expected, "{applied}");
}
fn duplicate_assessment(task: &Task, original: &Task, keep_context: bool) -> Value {
    let mut duplicated = assessment(task);
    if keep_context {
        duplicated["context_files_after"] = json!(task.context_files);
    }
    duplicated["duplicate_of"] =
        json!({"task_id": original.id, "evidence": "Both tasks make the same README edit."});
    duplicated
}
fn harness() -> (Workspace, String) {
    let ws = Workspace::new();
    let drain = ws.drain();
    (ws, drain)
}
fn harness_for(seconds: u64) -> (Workspace, String) {
    let ws = Workspace::new();
    let drain = ws.running(
        "workspace_auto_pipeline",
        json!({"approve_proposed": true, "for_seconds": seconds}),
    );
    (ws, drain)
}
fn assert_crew_partition(ws: &Workspace, drain: &str, first: &[&Task], deferred: &Task) {
    let selection = ws.select(drain);
    assert_task_ids(&selection, first);
    assert_eq!(
        selection["candidate_count"],
        json!(first.len()),
        "{selection}"
    );
    assert_eq!(selection["deferred_candidates"], 1, "{selection}");
    let pilot = ws.pilot_child(drain, &selection["task_ids"]);
    let assessments = first
        .iter()
        .map(|task| assessment(task))
        .collect::<Vec<_>>();
    ws.pilot_ok(drain, &pilot, assessments);
    for task in first {
        assert_status(ws, task, TaskStatus::Backlog);
    }
    assert_status(ws, deferred, TaskStatus::Proposed);
    let next = ws.select(drain);
    assert_task_ids(&next, &[deferred]);
    assert_eq!(next["candidate_count"], 1, "{next}");
    assert_eq!(next["deferred_candidates"], 0, "{next}");
}
fn explicit_pilot(ws: &Workspace, task: &Task) -> (String, Value) {
    let run = ws.running("task_pilot_pipeline", json!({"task_ids": [task.id]}));
    ws.set_trigger(&run, JobRunTrigger::cli());
    let prepared = ws.prepare_named(&run, std::slice::from_ref(&task.id));
    assert_eq!(prepared["task_count"], 1, "{prepared}");
    assert_eq!(
        prepared["partitions"],
        json!([{"partition_index": 0, "task_ids": [task.id]}]),
        "{prepared}"
    );
    (run, prepared)
}
fn apply_verified_outside(ws: &Workspace, assessments: Vec<Value>) {
    let pilot = ws.running("task_pilot_pipeline", json!({}));
    ws.apply_ok(&pilot, assessments, json!({}));
}
fn verified_cases(cases: &[(&Task, String)]) -> Vec<Value> {
    cases
        .iter()
        .map(|(task, evidence)| verified_no_diff(task, evidence))
        .collect()
}
fn assert_closed_sorted(selection: &Value, tasks: &[&Task]) {
    let mut closed = selection["closed"].as_array().unwrap().clone();
    closed.sort_by_key(|id| id.as_str().unwrap().to_string());
    let mut expected = tasks.iter().map(|task| json!(task.id)).collect::<Vec<_>>();
    expected.sort_by_key(|id| id.as_str().unwrap().to_string());
    assert_eq!(closed, expected, "{selection}");
}
fn assert_verified_no_diff_outside(
    ws: &Workspace,
    drain: &str,
    cases: Vec<Value>,
    closed: &[&Task],
    held: &[(&Task, Option<&str>)],
) -> Value {
    apply_verified_outside(ws, cases);
    let selection = ws.select(drain);
    assert_closed_sorted(&selection, closed);
    for task in closed {
        assert_status(ws, task, TaskStatus::Archived);
    }
    for (task, detail) in held {
        assert_status(ws, task, TaskStatus::Proposed);
        assert_held(&selection, task, "pilot_verified_no_diff");
        if let Some(detail) = detail {
            assert_held_detail(&selection, task, detail);
        }
    }
    selection
}
fn assert_boundary_selection(ws: &Workspace, drain: &str, legacy: &[&Task], forged: &Task) {
    let selection = ws.select(drain);
    for task in legacy {
        assert_held(&selection, task, "warnings");
    }
    assert_task_ids(&selection, &[forged]);
}
fn assert_tag_holds_at_drain(ws: &Workspace, drain: &str, task: &Task, reason: &str) {
    let selection = ws.select(drain);
    assert_eq!(selection["task_ids"], json!([]), "it costs no pilot work");
    assert_held(&selection, task, reason);
    let pilot = ws.pilot_child(drain, &json!([task.id]));
    let applied = ws.pilot_ok(drain, &pilot, vec![assessment(task)]);
    assert_classification(&applied, "drain_approval", reason);
    assert_status(ws, task, TaskStatus::Proposed);
}
fn assert_prepare_excluded(prepared: &Value, reason: &str) {
    assert_eq!(prepared["task_count"], 0, "{prepared}");
    assert_eq!(prepared["excluded_by_reason"][reason], 1, "{prepared}");
    assert_eq!(prepared["partitions"], json!([]), "{prepared}");
}
fn creation_hold_scenario(failed_pilot: bool) {
    let (ws, drain) = harness();
    let original = (!failed_pilot).then(|| ws.readme_task("original"));
    let creating = ws.creation_task("creating");
    let ordinary = ws.readme_task("ordinary");
    let reason = if failed_pilot {
        "pilot_unresolved"
    } else {
        "duplicate"
    };
    let selection = ws.select(&drain);
    if let Some(original) = &original {
        assert_task_ids(&selection, &[original, &creating, &ordinary]);
    } else {
        assert_task_ids(&selection, &[&creating, &ordinary]);
    }
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    if failed_pilot {
        ws.pilot_failed(&pilot);
    } else {
        let original = original.as_ref().unwrap();
        let assessments = vec![
            assessment(original),
            duplicate_assessment(&creating, original, true),
            duplicate_assessment(&ordinary, original, true),
        ];
        ws.pilot_ok(&drain, &pilot, assessments);
        assert_status(&ws, &creating, TaskStatus::Proposed);
    }
    assert_eq!(
        ws.last_event(&creating),
        CONTEXT_CREATION_AUTHORIZED_EVENT,
        "the pilot write re-seals the creation grant after its hold marker"
    );
    let assert_held_both = |selection: &Value| {
        assert_held(selection, &creating, reason);
        assert_held(selection, &ordinary, reason);
    };
    let next = ws.select(&drain);
    assert_task_ids(&next, &[]);
    assert_held_both(&next);
    if !failed_pilot {
        assert_eq!(next["held_by_reason"]["duplicate"], 2);
    }
    ws.comment(&creating);
    ws.comment(&ordinary);
    assert_eq!(ws.last_event(&creating), CONTEXT_CREATION_AUTHORIZED_EVENT);
    let next = ws.select(&drain);
    assert_task_ids(&next, &[]);
    assert_held_both(&next);
    ws.rename(&creating);
    ws.rename(&ordinary);
    assert_task_ids(&ws.select(&drain), &[&creating, &ordinary]);
    for edit in [
        Workspace::edit_description,
        Workspace::edit_plan,
        Workspace::edit_context,
    ] {
        let selection = ws.select(&drain);
        assert!(selected(&selection, &creating), "{selection}");
        let pilot = ws.pilot_child(&drain, &json!([creating.id.clone()]));
        if failed_pilot {
            ws.pilot_failed(&pilot);
        } else {
            let original = original.as_ref().unwrap();
            let assessments = vec![duplicate_assessment(&creating, original, true)];
            ws.pilot_ok(&drain, &pilot, assessments);
        }
        let held = ws.select(&drain);
        assert!(!selected(&held, &creating), "{held}");
        assert_held(&held, &creating, reason);
        edit(&ws, &creating);
        assert!(selected(&ws.select(&drain), &creating));
    }
}
fn assert_approved_by_drain(ws: &Workspace, tasks: &[&Task], drain: &str) {
    for task in tasks {
        assert_status(ws, task, TaskStatus::Backlog);
        let approval = ws.approval_entry(task);
        assert!(
            approval.note.as_deref().unwrap_or_default().contains(drain),
            "the approval names the drain: {approval:?}"
        );
    }
}
fn tasks_filed_mid_window_are_approved_by_tag_or_fields_and_admitted_the_same_pass() {
    let (ws, drain) = harness_for(3600);
    let tagged = ws.task(
        "tagged",
        &["no-diff-expected"],
        &[],
        TaskComplexity::Unassessed,
    );
    let scoped = ws.readme_task("scoped");
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&tagged, &scoped]);
    assert_eq!(selection["drain_promotion"], json!({"run_id": drain}));
    assert_eq!(selection["pilot_active"], true, "this pass starts a pilot");
    assert_eq!(selection["pilot_live"], false);
    assert_eq!(selection["pilot_finished"], false);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let late = ws.readme_task("late");
    let during = ws.select(&drain);
    assert_task_ids(&during, &[]);
    assert_eq!(during["pilot_live"], true);
    assert_eq!(during["live_pilot_run_ids"], json!([pilot]));
    assert_eq!(during["pilot_active"], true);
    assert_eq!(during["deferred_candidates"], 1, "{during}");
    for task in [&tagged, &scoped] {
        assert_held(&during, task, "pilot_running");
    }
    let mut no_diff = assessment(&tagged);
    no_diff["context_files_after"] = json!([]);
    no_diff["disposition"] = json!("verified_no_diff");
    no_diff["evidence"] = json!("The result is a filed report outside the repository.");
    let applied = ws.apply_ok(
        &pilot,
        vec![no_diff, assessment(&scoped)],
        json!({"drain_promotion": {"run_id": drain}}),
    );
    let decisions = applied["drain_approval"].as_array().unwrap();
    assert!(
        decisions
            .iter()
            .all(|d| d["decision"] == "promote" && d["approved"] == true),
        "{applied}"
    );
    assert_approved_by_drain(&ws, &[&tagged, &scoped], &drain);
    let still_running = ws.select(&drain);
    assert_eq!(still_running["pilot_finished"], false, "{still_running}");
    assert_task_ids(&still_running, &[]);
    ws.finish(&pilot, JobRunState::Success);
    let observing = ws.select(&drain);
    assert_eq!(observing["pilot_finished"], true, "{observing}");
    assert_eq!(observing["pilot_live"], false);
    assert_eq!(
        observing["task_ids"],
        json!([late.id]),
        "the next pilot starts"
    );
    let recorded = ws.record(&drain);
    assert_eq!(recorded["approved"], json!([tagged.id, scoped.id]));
    assert_eq!(recorded["approved_count"], 2);
    assert_eq!(recorded["pilots"][0]["run_id"], json!(pilot));
    assert_eq!(recorded["pilots"][0]["status"], "success");
    assert_eq!(ws.drain_report(&drain).approved_total, 2);
    let input = json!({"run_id": drain, "max_active_leaf_runs": 4});
    let admitted = ws.action("classify_workspace_auto_tasks", input).unwrap();
    let loose = admitted["loose_task_ids"].as_array().unwrap();
    assert!(
        loose.contains(&json!(scoped.id)),
        "the approved task is admitted in the pass that recorded it: {admitted}"
    );
    assert_eq!(ws.select(&drain)["pilot_finished"], false);
    assert_eq!(ws.record(&drain)["approved"], json!([]));
    assert_eq!(ws.drain_report(&drain).approved_total, 2);
}
fn unqualified_and_pilot_held_tasks_stay_proposed_with_their_reasons() {
    let (ws, drain) = harness();
    let unassessed = ws.task(
        "unassessed",
        &["no-pilot"],
        &["file:README.md"],
        TaskComplexity::Unassessed,
    );
    let bare = ws.task("bare", &["no-pilot"], &[], TaskComplexity::Low);
    let original = ws.readme_task("original");
    let duplicate = ws.readme_task("duplicate");
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&original, &duplicate]);
    assert_held(&selection, &unassessed, "unassessed_complexity");
    assert_held(&selection, &bare, "missing_context_files");
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let applied = ws.pilot_ok(
        &drain,
        &pilot,
        vec![
            assessment(&original),
            duplicate_assessment(&duplicate, &original, false),
        ],
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_status(&ws, &original, TaskStatus::Backlog);
    for task in [&unassessed, &bare, &duplicate] {
        assert_status(&ws, task, TaskStatus::Proposed);
    }
    let next = ws.select(&drain);
    assert_task_ids(&next, &[]);
    assert_held(&next, &duplicate, "duplicate");
    assert_eq!(next["held_by_reason"]["duplicate"], 1);
}
fn no_auto_approve_is_held_by_every_promotion_authority() {
    let (ws, drain) = harness();
    let opted_out = ws.tagged_task("opted out", &["no-auto-approve", "no-diff-expected"]);
    assert_tag_holds_at_drain(&ws, &drain, &opted_out, "no-auto-approve");
    let filed = ws.tagged_task(
        "ci failure",
        &[
            "ci-failure-sweep",
            "ci-failure:fixture-key",
            "no-auto-approve",
        ],
    );
    let applied = ws.sweep_ok(&filed, assessment(&filed));
    assert_classification(&applied, "ci_sweep_admission", "no-auto-approve");
    assert_status(&ws, &filed, TaskStatus::Proposed);
}
fn no_drain_is_held_by_drain_auto_approval() {
    let (ws, drain) = harness();
    let held = ws.tagged_task(
        "interactive-owned proposed",
        &["no-drain", "no-diff-expected"],
    );
    assert_tag_holds_at_drain(&ws, &drain, &held, "no-drain");
}
fn drain_authority_is_verified_before_any_approval() {
    let ws = Workspace::new();
    let task = ws.readme_task("scoped");
    let task_ids = json!([task.id]);
    let plain = ws.running("workspace_auto_pipeline", json!({}));
    let pilot = ws.pilot_child(&plain, &task_ids);
    let refused = ws
        .pilot(&plain, &pilot, vec![assessment(&task)])
        .unwrap_err();
    assert!(refused.contains("approve_proposed"), "{refused}");
    let drain = ws.drain();
    let stray = ws.running("task_pilot_pipeline", json!({ "task_ids": task_ids }));
    let refused = ws
        .pilot(&drain, &stray, vec![assessment(&task)])
        .unwrap_err();
    assert!(refused.contains("did not dispatch"), "{refused}");
    assert_status(&ws, &task, TaskStatus::Proposed);
}
fn a_pilot_hold_on_a_creation_target_task_survives_until_the_task_really_changes() {
    creation_hold_scenario(false);
}
fn an_unresolved_pilot_of_a_creation_target_task_is_retried_only_after_a_real_change() {
    creation_hold_scenario(true);
}
fn an_undecided_pilot_is_not_run_again_by_a_later_drain_until_the_task_really_changes() {
    let (ws, drain) = harness();
    let ordinary = ws.readme_task("ordinary");
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&ordinary]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    ws.pilot_failed(&pilot);
    ws.record(&drain);
    let later = ws.drain();
    let next = ws.select(&later);
    assert_task_ids(&next, &[]);
    assert_held(&next, &ordinary, "pilot_unresolved");
    assert_held_detail(
        &next,
        &ordinary,
        &format!("task-pilot run {pilot} left no decision"),
    );
    ws.rename(&ordinary);
    assert_task_ids(&ws.select(&later), &[&ordinary]);
}
/// A hold written before `task_pilot_unresolved` carried a typed decision
/// names its run only in the JSON note; the held reason still names it.
fn a_legacy_note_only_undecided_pilot_still_holds_with_its_run() {
    let (ws, drain) = harness();
    let ordinary = ws.readme_task("ordinary");
    append_unresolved_pilot_entry(&ws, &ordinary, "jrun-legacy-undecided");
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[]);
    assert_held(&selection, &ordinary, "pilot_unresolved");
    assert_held_detail(
        &selection,
        &ordinary,
        "task-pilot run jrun-legacy-undecided left no decision",
    );
}
fn a_pilot_that_could_not_draw_an_inspection_crew_is_retried_after_configuration() {
    let ws = Workspace::new();
    let ordinary = ws.readme_task("ordinary");
    let drain = ws.drain();
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&ordinary]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let now = Utc::now();
    ws.jobs().complete_job_run_step(&pilot, &JobRunStepParams {
        lock_wait_ms: None, step_index: 0, target_type: JobTargetType::Activity,
        target_id: "prepare_task_pilot".into(), started_at: now, finished_at: now,
        duration_ms: Some(0), exit_code: None, agent_response_json: None,
        state: JobRunState::Failed, error_code: None,
        error_message: Some("job executor: invalid input: inspection_crew_unavailable: no inspection-capable crew is configured".into()),
    }).unwrap();
    ws.finish(&pilot, JobRunState::Failed);
    ws.record(&drain);
    let history = ws.history(&ordinary);
    assert!(
        !history
            .iter()
            .any(|entry| entry.event == "task_pilot_unresolved")
    );
    append_unresolved_pilot_entry(&ws, &ordinary, &pilot);
    let mut executor: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/codex.yaml")).unwrap();
    executor.spec.inspection_args = Some(Vec::new());
    let def = ExecutorDef::from_resource_spec(executor.metadata.name, executor.spec, None, None);
    ws.runtime.upsert_executor_def(&def).unwrap();
    let later = ws.drain();
    assert_task_ids(&ws.select(&later), &[&ordinary]);
}
fn selected(selection: &Value, task: &Task) -> bool {
    selection["task_ids"]
        .as_array()
        .is_some_and(|ids| ids.contains(&json!(task.id)))
}
fn proposed_candidates_of_mixed_crews_are_selected_homogeneously_and_deferred() {
    let (ws, drain) = harness_for(3600);
    let task_a1 = ws.crewed_task("task_a1", Some("sol"));
    let task_b = ws.crewed_task("task_b", Some("grok"));
    let task_a2 = ws.crewed_task("task_a2", Some("sol"));
    assert_crew_partition(&ws, &drain, &[&task_a1, &task_a2], &task_b);
}
fn candidates_with_no_crew_are_never_bundled_with_crewed_candidates() {
    let (ws, drain) = harness_for(3600);
    let uncrewed_1 = ws.crewed_task("uncrewed_1", None);
    let crewed = ws.crewed_task("crewed", Some("sol"));
    let uncrewed_2 = ws.crewed_task("uncrewed_2", None);
    assert_crew_partition(&ws, &drain, &[&uncrewed_1, &uncrewed_2], &crewed);
}
fn verified_no_diff(task: &Task, evidence: &str) -> Value {
    let mut verified = assessment(task);
    verified["context_files_after"] = json!([]);
    verified["disposition"] = json!("verified_no_diff");
    verified["evidence"] = json!(evidence);
    verified["assessment_rationale"] = json!("The fix is already on the base branch.");
    verified
}
fn verified_no_diff_merge_proof_uses_only_the_first_parent_change() {
    let ws = Workspace::new();
    ws.branch_commit_file("unrelated", "side.txt", "unrelated\n", "unrelated");
    let culprit = ws.commit("the culprit");
    let unrelated_merge = ws.merge("unrelated", "merge unrelated branch");
    ws.branch_commit_file("touching", "README.md", "later fix\n", "later fix");
    let covering_merge = ws.merge("touching", "merge README fix");
    let drain = ws.drain();
    let finding = |title: &str| {
        ws.finding(
            title,
            format!("Introduced by commit {culprit}."),
            &["file:README.md"],
            Vec::new(),
        )
    };
    let unrelated = finding("unrelated merge does not cover the finding");
    let covered = finding("first-parent change covers the finding");
    let selection = assert_verified_no_diff_outside(
        &ws,
        &drain,
        verified_cases(&[
            (
                &unrelated,
                format!("Commit {unrelated_merge} already fixed the README."),
            ),
            (
                &covered,
                format!("Commit {covering_merge} already fixed the README."),
            ),
        ]),
        &[&covered],
        &[(&unrelated, None)],
    );
    assert_eq!(
        held_reason(&selection, &unrelated),
        Some("pilot_verified_no_diff"),
        "a merge's second-parent diff must not make it cover the finding: {selection}"
    );
}
fn an_auto_minted_verified_no_diff_is_archived_only_with_ancestor_proof() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let side = ws.side_commit();
    let drain = ws.drain();
    let sweep_tags = ["ci-failure-sweep", "ci-failure:fixture-key"];
    let covered = ws.tagged_task("covered", &sweep_tags);
    let off_base = ws.tagged_task("off base", &sweep_tags);
    let unknown = ws.tagged_task("unknown sha", &["auto-task:delivery-code-review"]);
    let uncited = ws.tagged_task("uncited", &["delivery-code-review"]);
    let selection = assert_verified_no_diff_outside(
        &ws,
        &drain,
        verified_cases(&[
            (
                &covered,
                format!("Commit {landed} already fixed the README."),
            ),
            (
                &off_base,
                format!("Commit {side} already fixed the README."),
            ),
            (
                &unknown,
                format!("Run 18234567890 passed after {landed} and 0badc0ffee1."),
            ),
            (
                &uncited,
                "The failing test does not exist at this revision.".to_string(),
            ),
        ]),
        &[&covered],
        &[
            (&off_base, Some(side.as_str())),
            (&unknown, Some("0badc0ffee1")),
            (&uncited, Some("no commit cited")),
        ],
    );
    assert_eq!(
        selection["task_ids"],
        json!([]),
        "no verified-no-diff task is piloted again"
    );
    let comment = ws.last_comment(&covered);
    assert!(
        comment.message.contains(&landed) && comment.message.contains("operation_id="),
        "the archive comment names the commit and the assessment: {comment:?}"
    );
    let report = ws.drain_report(&drain);
    assert_eq!(report.closed_total, 1);
    assert_eq!(report.closed, vec![covered.id.clone()]);
    assert_eq!(report.held_by_reason["pilot_verified_no_diff"], 3);
}
fn a_verified_no_diff_citing_only_the_findings_own_commits_is_held() {
    let ws = Workspace::new();
    let older = ws.commit("older change");
    let culprit = ws.commit("the culprit");
    let fix = ws.commit("the fix");
    let drain = ws.drain();
    let named = |title: &str| {
        format!(
            "{title}: introduced by {} on the base branch.",
            &culprit[..9]
        )
    };
    let regression = TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: ws.readme_task("culprit task").id,
        regression: None,
    };
    let finding = |title: &str, relations: Vec<TaskRelation>| {
        ws.readme_finding(title, named(title), relations)
    };
    let own_culprit = finding("own culprit", vec![regression.clone()]);
    let fixed = finding("fixed after culprit", vec![regression.clone()]);
    let before_culprit = finding("cites older commit", vec![regression]);
    let other_path = ws.finding(
        "other path",
        "A finding that names no commit.".into(),
        &["file:src/other.rs"],
        Vec::new(),
    );
    let related = ws.readme_finding(
        "related change",
        "A finding that names no commit.".into(),
        Vec::new(),
    );
    assert_verified_no_diff_outside(
        &ws,
        &drain,
        verified_cases(&[
            // Quoting the culprit the finding names is not a fix.
            (&own_culprit, format!("Commit {culprit} is the culprit.")),
            (
                &fixed,
                format!("Commit {culprit} broke it and {fix} repaired the README."),
            ),
            (
                &before_culprit,
                format!("Commit {older} touched the README."),
            ),
            (&other_path, format!("Commit {fix} repaired the README.")),
            (&related, format!("Commit {fix} repaired the README.")),
        ]),
        &[&fixed, &related],
        &[
            (&own_culprit, None),
            (&before_culprit, None),
            (&other_path, None),
        ],
    );
    let comment = ws.last_comment(&fixed);
    assert!(
        comment.message.contains("Covering commit(s)")
            && comment.message.contains(&format!(": {fix}.")),
        "only the later commit is named as covering: {comment:?}"
    );
}
fn a_culprit_named_by_an_all_digit_abbreviation_still_does_not_count() {
    let ws = Workspace::new();
    // About 4% of 7-char abbreviations are all digits: recommit until one is.
    let culprit = (0..1000)
        .map(|attempt| ws.commit(&format!("the culprit {attempt}")))
        .find(|sha| sha[..7].bytes().all(|byte| byte.is_ascii_digit()))
        .expect("an all-digit abbreviation within 1000 commits");
    let fix = ws.commit("the fix");
    let drain = ws.drain();
    let description = format!("Introduced by {} on the base branch.", &culprit[..7]);
    let own_culprit = ws.readme_finding("own culprit", description.clone(), Vec::new());
    let fixed = ws.readme_finding("fixed after culprit", description, Vec::new());
    assert_verified_no_diff_outside(
        &ws,
        &drain,
        verified_cases(&[
            (&own_culprit, format!("Commit {culprit} is the culprit.")),
            (&fixed, format!("Commit {fix} repaired the README.")),
        ]),
        &[&fixed],
        &[(&own_culprit, None)],
    );
}
fn an_orchestrator_filed_verified_no_diff_is_held_with_its_evidence_and_never_closed() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let drain = ws.drain();
    let filed = ws.tagged_task("filed", &["drain-robustness"]);
    let evidence = format!("Commit {landed} already carries the README change.");
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&filed]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let applied = ws.pilot_ok(&drain, &pilot, vec![verified_no_diff(&filed, &evidence)]);
    let decision = &applied["drain_approval"][0];
    assert_classification(&applied, "drain_approval", "pilot_verified_no_diff");
    assert_eq!(decision["evidence"]["cited_commits"], json!([landed]));
    assert_eq!(decision["closed"], false, "{applied}");
    let next = ws.select(&drain);
    assert_status(&ws, &filed, TaskStatus::Proposed);
    assert_eq!(next["closed"], json!([]), "{next}");
    assert_held(&next, &filed, "pilot_verified_no_diff");
    assert_held_detail(&next, &filed, &evidence);
    let report = ws.drain_report(&drain);
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
    let readiness = ws
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_held(&readiness["approvals"], &filed, "pilot_verified_no_diff");
    assert_held_detail(&readiness["approvals"], &filed, &evidence);
    assert_status(&ws, &filed, TaskStatus::Proposed);
}
fn the_ci_sweep_closes_a_verified_no_diff_by_the_same_proof() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let tags = ["ci-failure-sweep", "ci-failure:fixture-key"];
    let covered = ws.tagged_task("covered", &tags);
    let applied = ws.sweep_ok(
        &covered,
        verified_no_diff(&covered, &format!("Fixed by {landed}.")),
    );
    let decision = &applied["ci_sweep_admission"][0];
    assert_classification(&applied, "ci_sweep_admission", "pilot_verified_no_diff");
    assert_eq!(decision["closed"], true, "{applied}");
    assert_eq!(decision["covering_commits"], json!([landed]));
    assert_status(&ws, &covered, TaskStatus::Archived);
    let uncited = ws.tagged_task("uncited", &tags);
    let applied = ws.sweep_ok(
        &uncited,
        verified_no_diff(
            &uncited,
            "The failing test does not exist at this revision.",
        ),
    );
    let decision = &applied["ci_sweep_admission"][0];
    assert_classification(&applied, "ci_sweep_admission", "pilot_verified_no_diff");
    assert_eq!(decision["closed"], false, "{applied}");
    assert_status(&ws, &uncited, TaskStatus::Proposed);
}
fn append_history(
    ws: &Workspace,
    task: &Task,
    actor: &str,
    at: DateTime<Utc>,
    event: &str,
    note: String,
) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&ws.runtime.global_root()),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        ws.runtime.workspace_id().unwrap(),
        ws.runtime.sqlite_store().unwrap(),
    )
    .unwrap();
    let entry = orbit_types::task::TaskHistoryEntry {
        at,
        by: actor.into(),
        event: event.into(),
        note: Some(note),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    };
    let params = orbit_store::contracts::TaskHistoryUpdateParams {
        actor: actor.into(),
        append_history: vec![entry],
        ..Default::default()
    };
    backends
        .task
        .history
        .update_task_history(&task.id, params)
        .unwrap();
}
fn append_pilot_entry(ws: &Workspace, task: &Task, at: &str, note: &str) {
    let at: DateTime<Utc> = at.parse().unwrap();
    append_history(
        ws,
        task,
        "task-pilot",
        at,
        "task_pilot_applied",
        note.into(),
    );
}
/// The note-only shape `task_pilot_unresolved` had before its typed decision.
fn append_unresolved_pilot_entry(ws: &Workspace, task: &Task, pilot_run_id: &str) {
    let note = json!({"pilot_run_id": pilot_run_id}).to_string();
    append_history(
        ws,
        task,
        "system",
        Utc::now(),
        "task_pilot_unresolved",
        note,
    );
}

fn a_drain_hold_note_counts_only_when_written_before_the_store_recorded_typed_decisions() {
    let first_open = Utc::now();
    let (ws, drain) = harness();
    let note = "selectors (operation_id=legacy-pilot) [drain-approval-held:warnings]";
    let legacy = [
        "2026-10-01T00:00:00Z".to_string(),
        "2026-10-12T09:00:00Z".to_string(),
        (first_open - chrono::Duration::seconds(1)).to_rfc3339(),
    ]
    .iter()
    .enumerate()
    .map(|(index, at)| {
        let task = ws.readme_task(&format!("legacy {index}"));
        append_pilot_entry(&ws, &task, at, note);
        task
    })
    .collect::<Vec<_>>();
    let forged = ws.readme_task("forged");
    append_pilot_entry(&ws, &forged, &Utc::now().to_rfc3339(), note);
    OrbitRuntime::from_roots(&ws.runtime.global_root(), &ws.repo.join(".orbit")).unwrap();
    assert_boundary_selection(&ws, &drain, &legacy.iter().collect::<Vec<_>>(), &forged);
}

/// The store's typed-decisions boundary file.
pub(crate) fn typed_decisions_boundary(global_root: &Path) -> PathBuf {
    orbit_store::maintenance::task_registry::task_registry_path(global_root)
        .with_file_name("typed-decisions.since")
}

/// Leave the boundary file holding `content`; return its modification time.
pub(crate) fn seed_incomplete_boundary(global_root: &Path, content: &str) -> DateTime<Utc> {
    let path = typed_decisions_boundary(global_root);
    std::fs::write(&path, content).unwrap();
    let modified = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(modified).unwrap();
    modified.into()
}

/// The boundary file's recorded instant.
pub(crate) fn recorded_boundary(global_root: &Path) -> DateTime<Utc> {
    std::fs::read_to_string(typed_decisions_boundary(global_root))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn an_incomplete_boundary_counts_from_its_modification_time_until_a_writable_open_repairs_it() {
    let note = "selectors (operation_id=legacy-pilot) [drain-approval-held:warnings]";
    for content in ["", "2026-10-12T1"] {
        let (ws, drain) = harness();
        let global_root = ws.runtime.global_root();
        let modified = seed_incomplete_boundary(&global_root, content);
        let legacy = ws.readme_task("legacy");
        append_pilot_entry(&ws, &legacy, "2026-10-12T09:00:00Z", note);
        let forged = ws.readme_task("forged");
        append_pilot_entry(&ws, &forged, &Utc::now().to_rfc3339(), note);
        assert_boundary_selection(&ws, &drain, &[&legacy], &forged);
        OrbitRuntime::from_roots(&global_root, &ws.repo.join(".orbit")).unwrap();
        assert_eq!(recorded_boundary(&global_root), modified, "{content:?}");
        assert_boundary_selection(&ws, &drain, &[&legacy], &forged);
    }
}

fn explicit_assessment_is_promoted_once_while_the_drain_child_stands_down() {
    let (ws, drain) = harness_for(3600);
    let assessed = ws.readme_task("assessed");
    let (explicit, prepared) = explicit_pilot(&ws, &assessed);
    ws.checkpoint_prepare(&explicit, &prepared);
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[]);
    assert_held(&selection, &assessed, "pilot_running");
    let child = ws.pilot_child(&drain, &json!([assessed.id]));
    ws.set_trigger(&child, JobRunTrigger::child());
    let stood_down = ws.prepare_named(&child, std::slice::from_ref(&assessed.id));
    assert_prepare_excluded(&stood_down, "already_preparing");
    let applied = ws.apply_unpromoted(&explicit, &prepared, &assessed);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_status(&ws, &assessed, TaskStatus::Proposed);
    assert_eq!(ws.pilot_applications(&assessed), 1);
    ws.finish(&explicit, JobRunState::Success);
    let reused = ws.prepare_named(&child, std::slice::from_ref(&assessed.id));
    assert_prepare_excluded(&reused, "assessment_already_applied");
    assert_eq!(ws.pilot_applications(&assessed), 1);
    ws.finish(&child, JobRunState::Success);
    let recorded = ws.record(&drain);
    assert_eq!(
        recorded["awaiting_reuse"],
        json!([assessed.id]),
        "{recorded}"
    );
    assert!(
        ws.history(&assessed)
            .iter()
            .all(|entry| entry.event != "task_pilot_unresolved"),
        "a child that stood down must not record the explicit assessment as unresolved"
    );
    let unauthorized = ws.running("workspace_auto_pipeline", json!({}));
    let forged = ws.select(&unauthorized);
    assert_eq!(forged["reused"], json!([]), "{forged}");
    assert_eq!(forged["awaiting_reuse"], json!([assessed.id]), "{forged}");
    assert_status(&ws, &assessed, TaskStatus::Proposed);
    let fresh = ws.readme_task("fresh");
    let promoted = ws.select(&drain);
    assert_task_ids(&promoted, &[&fresh]);
    assert_eq!(promoted["reused"], json!([assessed.id]), "{promoted}");
    assert_status(&ws, &assessed, TaskStatus::Backlog);
    assert_eq!(ws.pilot_applications(&assessed), 1);
    let approval = ws.approval_entry(&assessed);
    let prefix = format!("approved by auto drain {drain} after task-pilot verification");
    assert!(
        approval
            .note
            .as_deref()
            .unwrap_or_default()
            .starts_with(&prefix),
        "{approval:?}"
    );
    let again = ws.select(&drain);
    assert_task_ids(&again, &[&fresh]);
    assert_eq!(again["reused"], json!([]), "{again}");
    assert_eq!(ws.pilot_applications(&assessed), 1);
}

fn record_execution_claim(ws: &Workspace, task: &Task) {
    let claim = orbit_store::contracts::ExecutionClaim {
        claim_id: "stale-repilot-claim".into(),
        task_id: task.id.clone(),
        request_id: "stale-repilot-request".into(),
        executed_on: orbit_store::contracts::ExecutionLocation {
            machine_id: "fixture-machine".into(),
            machine_name: None,
        },
        run_context: orbit_store::contracts::AdmissionRunContext {
            run_id: "fixture-claim-run".into(),
            job_name: "task_pr_pipeline".into(),
            machine_name: None,
        },
        footprint: Vec::new(),
        reservation_id: "stale-repilot-reservation".into(),
        reservation_expires_at: (Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
        phase: orbit_store::contracts::ExecutionClaimPhase::Running,
        repair: None,
    };
    let connection = rusqlite::Connection::open(ws.runtime.global_root().join("orbit.db")).unwrap();
    connection
        .execute(
            "INSERT OR REPLACE INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
             VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
            rusqlite::params![
                ws.runtime.workspace_id().unwrap(),
                claim.claim_id,
                serde_json::to_string(&claim).unwrap(),
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
}

/// Apply one unpromoted explicit assessment, then edit the description so
/// the certified assessment is stale. Returns the task in that state.
fn stale_explicit_task(ws: &Workspace) -> Task {
    let task = ws.readme_task("edited");
    let (explicit, prepared) = explicit_pilot(ws, &task);
    let applied = ws.apply_unpromoted(&explicit, &prepared, &task);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    ws.finish(&explicit, JobRunState::Success);
    ws.edit_description(&task);
    assert_status(ws, &task, TaskStatus::Proposed);
    assert_eq!(ws.pilot_applications(&task), 1);
    task
}

fn a_stale_explicit_assessment_is_repiloted_by_the_drain_and_admitted() {
    let (ws, drain) = harness();
    let task = stale_explicit_task(&ws);
    // No operator repilot: the drain queues a fresh pilot like a new proposal.
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&task]);
    assert_eq!(selection["reused"], json!([]), "{selection}");
    assert!(held_reason(&selection, &task).is_none(), "{selection}");
    // The dispatched child assesses the edited task instead of standing down.
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    ws.set_trigger(&pilot, JobRunTrigger::child());
    let prepared = ws.prepare_named(&pilot, std::slice::from_ref(&task.id));
    assert_eq!(prepared["task_count"], 1, "{prepared}");
    let applied = ws
        .action(
            "apply_task_pilot_results",
            json!({
                "run_id": pilot, "workspace_path": ws.repo, "prepared": prepared,
                "results": [{
                    "partition_index": 0, "task_ids": [task.id],
                    "tasks": [assessment(&task)],
                }],
                "promotion_authorized": true,
                "drain_promotion": {"run_id": drain},
            }),
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    ws.finish(&pilot, JobRunState::Success);
    assert_eq!(ws.pilot_applications(&task), 2);
    assert_status(&ws, &task, TaskStatus::Backlog);
}

fn a_failed_repilot_of_a_stale_assessment_still_holds() {
    let (ws, drain) = harness();
    let task = stale_explicit_task(&ws);
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&task]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    ws.pilot_failed(&pilot);
    ws.record(&drain);
    let next = ws.select(&drain);
    assert_task_ids(&next, &[]);
    assert_held(&next, &task, "pilot_unresolved");
    assert_status(&ws, &task, TaskStatus::Proposed);
    // A later drain holds it too, until the task changes or an operator acts.
    let later = ws.drain();
    let held = ws.select(&later);
    assert_task_ids(&held, &[]);
    assert_held(&held, &task, "pilot_unresolved");
}

fn a_stale_assessment_on_a_claimed_task_still_holds() {
    let (ws, drain) = harness();
    let task = stale_explicit_task(&ws);
    record_execution_claim(&ws, &task);
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[]);
    assert_eq!(selection["reused"], json!([]), "{selection}");
    assert_held(&selection, &task, "changed_since_pilot");
    assert_status(&ws, &task, TaskStatus::Proposed);
    assert_eq!(ws.pilot_applications(&task), 1);
}

crate::scenario_table!(
    explicit_assessment_reuse_scenarios,
    explicit_assessment_is_promoted_once_while_the_drain_child_stands_down,
    a_stale_explicit_assessment_is_repiloted_by_the_drain_and_admitted,
    a_failed_repilot_of_a_stale_assessment_still_holds,
    a_stale_assessment_on_a_claimed_task_still_holds
);
crate::scenario_table!(
    tasks_filed_mid_window_are_approved_by_tag_or_fields_and_admitted_the_same_pass_scenarios,
    tasks_filed_mid_window_are_approved_by_tag_or_fields_and_admitted_the_same_pass,
    unqualified_and_pilot_held_tasks_stay_proposed_with_their_reasons,
    no_auto_approve_is_held_by_every_promotion_authority,
    no_drain_is_held_by_drain_auto_approval,
    drain_authority_is_verified_before_any_approval,
    a_pilot_hold_on_a_creation_target_task_survives_until_the_task_really_changes,
    an_unresolved_pilot_of_a_creation_target_task_is_retried_only_after_a_real_change,
    an_undecided_pilot_is_not_run_again_by_a_later_drain_until_the_task_really_changes,
    a_legacy_note_only_undecided_pilot_still_holds_with_its_run,
    a_pilot_that_could_not_draw_an_inspection_crew_is_retried_after_configuration,
    proposed_candidates_of_mixed_crews_are_selected_homogeneously_and_deferred,
    candidates_with_no_crew_are_never_bundled_with_crewed_candidates,
    verified_no_diff_merge_proof_uses_only_the_first_parent_change,
    an_auto_minted_verified_no_diff_is_archived_only_with_ancestor_proof,
    a_verified_no_diff_citing_only_the_findings_own_commits_is_held
);
crate::scenario_table!(
    a_culprit_named_by_an_all_digit_abbreviation_still_does_not_count_scenarios,
    a_culprit_named_by_an_all_digit_abbreviation_still_does_not_count,
    an_orchestrator_filed_verified_no_diff_is_held_with_its_evidence_and_never_closed,
    the_ci_sweep_closes_a_verified_no_diff_by_the_same_proof,
    a_drain_hold_note_counts_only_when_written_before_the_store_recorded_typed_decisions,
    an_incomplete_boundary_counts_from_its_modification_time_until_a_writable_open_repairs_it
);
