//! The blocked-task triage through its public boundaries: an operator
//! auto-task's `skip_unless_blocked_candidates` precondition mints only for
//! an episode awaiting triage, and `orbit.task.block_triage` — the tool the
//! `block_escalated` escalation triager records with — records exactly one
//! disposition per episode — a requeue counted against the requeue bound
//! final recovery shares, a decision proposal that changes nothing else, or a
//! human-only report — and refuses anything else. The seeded
//! `blocked_task_triage` routine's `block_escalated` trigger admits each
//! qualifying episode once, under the same rule.

use std::path::PathBuf;

use chrono::{Duration, Utc};
use orbit_core::application::auto_tasks::state::{cursor_state_path, load_cursor_state};
use orbit_core::application::auto_tasks::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_core::application::automation::evaluate_routine;
use orbit_core::application::task::{FinalRecoveryCompletion, FinalRecoveryRequest};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{ExecutionLocation, TaskStatus};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

const DEFINITION: &str = "block-reader";
/// An operator's own blocked-task reader: the precondition is a definition
/// field any auto-task may set, now that the routine replaced the shipped
/// default.
const OPERATOR_DEFINITION: &str = "\
schemaVersion: 1
name: block-reader
enabled: true
schedule:
  every_minutes: 1
dedupe: skip_if_open
skip_unless_blocked_candidates: true
template:
  title: Read escalated blocks
  description: Triage each block episode once.
  acceptance_criteria:
  - Every qualifying episode has one disposition.
  complexity: medium
  tags:
  - block-reader
";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    runs: std::cell::Cell<u32>,
}

fn fixture() -> Fixture {
    fixture_with_config("")
}

fn fixture_with_config(config: &str) -> Fixture {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit/auto_tasks")).unwrap();
    if !config.is_empty() {
        std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
    }
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    Fixture {
        _root: root,
        global,
        repo,
        runtime,
        runs: std::cell::Cell::new(0),
    }
}

impl Fixture {
    /// A runtime acting for a named human, over the same roots.
    fn human(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_actor(ActorIdentity::human("human:operator"))
    }

    fn task(&self, tags: &[&str]) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Recover an escalated block",
                    "description": "Blocked-task triage fixture task.",
                    "acceptance_criteria": ["Recovered."],
                    "complexity": "low",
                    "type": "chore",
                    "tags": tags,
                    "workspace": self.repo.to_string_lossy(),
                    "model": "codex"
                }),
            )
            .unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        self.update(json!({"id": id, "plan": "1. Do it."}));
        self.update(json!({"id": id, "status": "backlog"}));
        id
    }

    fn update(&self, mut input: Value) {
        input["model"] = json!("codex");
        self.runtime.run_tool("orbit.task.update", input).unwrap();
    }

    fn next_run(&self) -> String {
        self.runs.set(self.runs.get() + 1);
        format!("jrun-triage-{}", self.runs.get())
    }

    /// Final recovery's `decision` on a fresh run of the in-progress `task`.
    fn final_recovery(&self, task: &str, decision: Value) {
        let observed = self
            .runtime
            .final_recovery_revision(&self.runtime.get_task(task).unwrap())
            .unwrap();
        self.runtime
            .apply_final_recovery(
                &FinalRecoveryRequest {
                    task_id: task.to_string(),
                    run_id: self.next_run(),
                    observed,
                    repo_root: self.repo.clone(),
                    base_ref: "main".to_string(),
                    completion: FinalRecoveryCompletion::Review,
                    requeue_bound: Default::default(),
                },
                Some(&decision),
            )
            .unwrap();
    }

    /// Start `task` and let final recovery escalate its failed run: one new
    /// escalated block episode.
    fn escalate(&self, task: &str) {
        self.update(json!({"id": task, "status": "in_progress"}));
        self.final_recovery(
            task,
            json!({
                "decision": "escalate",
                "diagnosis": "the worker crashed while the host was rebooting",
                "human_action": "requeue once the host is back",
            }),
        );
        assert_eq!(self.status(task), "blocked");
    }

    fn status(&self, task: &str) -> String {
        self.show(task)["status"].as_str().unwrap().to_string()
    }

    fn show(&self, task: &str) -> Value {
        self.runtime
            .run_tool("orbit.task.show", json!({ "id": task }))
            .unwrap()
    }

    fn triage(&self, input: Value) -> Result<Value, String> {
        let mut input = input;
        input["model"] = json!("claude");
        input["workspace"] = json!(self.repo.to_string_lossy());
        self.runtime
            .run_tool("orbit.task.block_triage", input)
            .map_err(|error| error.to_string())
    }

    fn requeue(&self, task: &str) -> Result<Value, String> {
        self.triage(json!({
            "action": "requeue",
            "id": task,
            "blocker": "the worker crashed while the host was rebooting",
            "reason": "the host is back and a later run on it succeeded",
        }))
    }

    fn candidates(&self) -> Value {
        self.triage(json!({"action": "candidates"})).unwrap()
    }

    fn qualifying(&self) -> Vec<String> {
        self.candidates()["qualifying"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["task_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn excluded_why(&self, task: &str) -> String {
        self.candidates()["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["task_id"] == task)
            .unwrap_or_else(|| panic!("{task} is listed as excluded"))["why"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Hold `task` in a live escalation triage run with a real successful
    /// prepare checkpoint, as a run is while its triagers work.
    fn hold_in_triage_run(&self, task: &str) -> String {
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run("escalation_triage_pipeline", 1, Utc::now(), None, None)
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let prepared = self
            .runtime
            .run_deterministic(
                "prepare_escalation_triage",
                &json!({}),
                &json!({
                    "subject": "block", "task_ids": [task],
                    "workspace_path": self.repo, "base_branch": "main",
                    "run_id": run.run_id,
                }),
                orbit_tools::ToolContext::default(),
            )
            .unwrap();
        assert_eq!(prepared["task_ids"], json!([task]), "{prepared}");
        let mut state = PipelineState::new(
            run.run_id.clone(),
            "escalation_triage_pipeline".into(),
            json!({}),
        );
        state.record_step(0, JobRunState::Success, Some(prepared), None);
        self.runtime.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    fn triage_comments(&self, task: &str) -> Vec<String> {
        self.show(task)["comments"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|comment| comment["message"].as_str())
            .filter(|message| message.starts_with("blocked_task_triage "))
            .map(str::to_string)
            .collect()
    }

    /// The operator's definition, enabled and due every minute.
    fn install_definition(&self) {
        std::fs::write(
            self.repo
                .join(format!(".orbit/auto_tasks/{DEFINITION}.yaml")),
            OPERATOR_DEFINITION,
        )
        .unwrap();
    }

    fn tick(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> orbit_core::application::auto_tasks::AutoTaskFireReport {
        let mut outcome =
            run_auto_task_scheduler_at(&self.runtime, now, SchedulerOptions::default()).unwrap();
        assert!(outcome.errors.is_empty(), "{outcome:?}");
        assert_eq!(outcome.reports.len(), 1, "{outcome:?}");
        outcome.reports.remove(0)
    }

    fn minted(&self) -> Vec<Value> {
        self.runtime
            .run_tool(
                "orbit.task.list",
                json!({"tag": [DEFINITION], "workspace": self.repo.to_string_lossy()}),
            )
            .unwrap()["tasks"]
            .as_array()
            .cloned()
            .unwrap()
    }
}

#[test]
fn the_precondition_skips_without_a_qualifying_episode_and_mints_once_for_one() {
    let fx = fixture();
    fx.install_definition();
    let now = Utc::now();
    assert_eq!(fx.tick(now).action, "baselined");

    // A task blocked by automation with no decision is still the backstop's
    // for 72 hours; before then nothing awaits triage.
    let undecided = fx.task(&[]);
    fx.update(json!({"id": undecided, "status": "in_progress"}));
    fx.update(json!({"id": undecided, "status": "blocked", "note": "worker exited"}));
    let skipped = fx.tick(now + Duration::minutes(2));
    assert_eq!(skipped.action, "skipped", "{skipped:?}");
    let state = load_cursor_state(&cursor_state_path(&fx.runtime.paths().state_dir)).unwrap();
    let skip = state.definitions[DEFINITION].last_skip.as_ref().unwrap();
    assert_eq!(skip.reason, "no_blocked_candidates");
    assert_eq!(
        skip.detail.as_deref(),
        Some("1 blocked task(s) read; no block episode awaits triage")
    );
    assert!(fx.minted().is_empty(), "a skipped fire mints nothing");
    let shown = fx
        .runtime
        .run_tool("orbit.auto_task.show", json!({"name": DEFINITION}))
        .unwrap();
    assert_eq!(
        shown["cursor"]["last_skip"]["reason"],
        "no_blocked_candidates"
    );
    assert_eq!(shown["skip_unless_blocked_candidates"], true);

    let escalated = fx.task(&[]);
    fx.escalate(&escalated);
    let fired = fx.tick(now + Duration::minutes(4));
    assert_eq!(fired.action, "fired", "{fired:?}");
    let minted = fx.minted();
    assert_eq!(minted.len(), 1, "exactly one triage task: {minted:?}");
    assert_eq!(fired.task_id.as_deref(), minted[0]["id"].as_str());
    // The open triage task dedupes the next due fire.
    assert_eq!(fx.tick(now + Duration::minutes(6)).action, "skipped");
    assert_eq!(fx.minted().len(), 1);

    // Past the backstop's limit, an undecided block qualifies too.
    assert_eq!(
        fx.qualifying(),
        vec![escalated.clone()],
        "a block under 72 hours with no decision does not qualify"
    );
    let late = fx
        .runtime
        .blocked_triage_view(now + Duration::hours(73))
        .unwrap();
    let undecided_late = late.iter().find(|view| view.task.id == undecided).unwrap();
    assert!(
        matches!(
            undecided_late.candidacy,
            orbit_core::application::task::TriageCandidacy::Qualifies(_)
        ),
        "{:?}",
        undecided_late.candidacy
    );
}

#[test]
fn a_requeue_counts_against_the_bound_final_recovery_shares() {
    let fx = fixture();
    let task = fx.task(&[]);
    // Final recovery's own requeue is the first of the two the window allows.
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.final_recovery(
        &task,
        json!({"decision": "requeue", "reason": "the missing tool now resolves"}),
    );
    assert_eq!(fx.status(&task), "backlog");

    fx.escalate(&task);
    let row = &fx.candidates()["qualifying"][0];
    assert_eq!(row["task_id"], task.as_str());
    assert_eq!(row["requeues_in_window"], 1);
    assert_eq!(row["basis"], "escalated");
    assert!(
        row["escalation"]
            .as_str()
            .is_some_and(|comment| comment.contains("outcome=escalated")),
        "the candidate quotes the escalation: {row}"
    );
    let requeued = fx.requeue(&task).unwrap();
    assert_eq!(requeued["outcome"], "requeued");
    assert_eq!(requeued["requeue"]["summary"], "requeue 2 of 2 per 24h");
    assert_eq!(fx.status(&task), "backlog");
    let comments = fx.triage_comments(&task);
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0].contains("requeue 2 of 2 per 24h")
            && comments[0].contains("> the worker crashed while the host was rebooting"),
        "{}",
        comments[0]
    );

    // A third requeue in the window is refused, and the task stays blocked
    // with the bound named.
    fx.escalate(&task);
    let refused = fx.requeue(&task).unwrap();
    assert_eq!(refused["outcome"], "refused");
    assert_eq!(fx.status(&task), "blocked");
    let refusal = refused["refusal"].as_str().unwrap();
    assert!(
        refusal.contains("limit 2 per 24 hour(s), shared with final recovery"),
        "{refusal}"
    );
    assert!(fx.triage_comments(&task)[1].contains(refusal));
    // The refusal triaged this episode; asking again records nothing.
    let again = fx.requeue(&task).unwrap_err();
    assert!(
        again.contains("no block episode awaiting triage"),
        "{again}"
    );
    assert_eq!(fx.triage_comments(&task).len(), 2);
}

#[test]
fn a_decision_proposal_writes_one_comment_and_changes_nothing_else() {
    let fx = fixture();
    let task = fx.task(&["scope"]);
    fx.escalate(&task);
    let before = fx.runtime.get_task(&task).unwrap();
    let proposal = fx
        .triage(json!({
            "action": "propose",
            "id": task,
            "blocker": "criterion 2 contradicts the retired API",
            "options": [
                {"summary": "Drop criterion 2", "consequences": "the old API stays retired", "replacement": "- Recovered."},
                {"summary": "Keep the API", "consequences": "the retirement is reverted", "replacement": "- Recovered.\n- The API answers."},
            ],
            "recommended": 1,
        }))
        .unwrap();
    assert_eq!(proposal["disposition"], "decision");
    assert_eq!(proposal["outcome"], "proposed");
    let after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(after.status, TaskStatus::Blocked);
    assert_eq!(after.description, before.description);
    assert_eq!(after.acceptance_criteria, before.acceptance_criteria);
    assert_eq!(after.context_files, before.context_files);
    assert_eq!(after.tags, before.tags);
    assert_eq!(after.plan, before.plan);
    let [comment] = fx.triage_comments(&task).try_into().unwrap();
    for part in [
        "> criterion 2 contradicts the retired API",
        "Option 1: Drop criterion 2",
        "Consequences: the retirement is reverted",
        "Replacement text:\n- Recovered.\n- The API answers.",
        "Recommended: option 1 (Drop criterion 2)",
    ] {
        assert!(comment.contains(part), "{part:?} in {comment}");
    }

    // One option is not a decision.
    let other = fx.task(&[]);
    fx.escalate(&other);
    let error = fx
        .triage(json!({
            "action": "propose",
            "id": other,
            "blocker": "ambiguous",
            "options": [{"summary": "a", "consequences": "b", "replacement": "c"}],
            "recommended": 1,
        }))
        .unwrap_err();
    assert!(error.contains("two or three"), "{error}");
    assert!(fx.triage_comments(&other).is_empty());
}

/// Human-only blocks: a report changes nothing but its comment. Security tags
/// and a foreign run are refused at the requeue boundary itself. Credentials,
/// a named owner sign-off and an unmet OS or browser-kit requirement are the
/// agent's reading of the evidence, recorded through the same `report` call
/// this fixture exercises.
#[test]
fn human_only_blocks_are_reported_and_never_requeued() {
    let fx = fixture();
    let credentials = fx.task(&[]);
    fx.escalate(&credentials);
    let reported = fx
        .triage(json!({
            "action": "report",
            "id": credentials,
            "blocker": "provider login expired",
            "reason": "the operator signs the provider in again",
        }))
        .unwrap();
    assert_eq!(reported["disposition"], "human_only");
    assert_eq!(reported["outcome"], "reported");
    assert_eq!(fx.status(&credentials), "blocked");
    assert!(
        fx.triage_comments(&credentials)[0]
            .contains("human_action: the operator signs the provider in again")
    );

    for tag in ["security", "security-review"] {
        let task = fx.task(&[tag]);
        fx.escalate(&task);
        let refused = fx.requeue(&task).unwrap();
        assert_eq!(refused["outcome"], "refused", "{tag}");
        assert_eq!(fx.status(&task), "blocked", "{tag}");
        assert!(
            refused["refusal"].as_str().unwrap().contains(tag),
            "{refused}"
        );
    }

    // A run another machine executed binds the task to that machine.
    let foreign = fx.task(&[]);
    let jobs = orbit_store::compose::workspace_job_run_store(
        fx.runtime.sqlite_store().unwrap(),
        fx.runtime.workspace_id().unwrap(),
    )
    .with_execution_location(Some(ExecutionLocation {
        machine_id: "mch-follower".into(),
        machine_name: Some("follower".into()),
    }));
    let run = jobs
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(1))
        .unwrap();
    RuntimeHost::apply_task_automation_update(
        &fx.runtime,
        &foreign,
        TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress),
            job_run_id: Some(run.run_id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    fx.final_recovery(
        &foreign,
        json!({"decision": "escalate", "diagnosis": "the follower lost its sandbox", "human_action": "inspect the follower"}),
    );
    let row = fx.candidates()["qualifying"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["task_id"] == foreign.as_str())
        .cloned()
        .unwrap();
    assert_eq!(row["run_is_local"], false, "{row}");
    assert!(
        row["requeue_refusal"]
            .as_str()
            .is_some_and(|r| r.contains("mch-follower")),
        "{row}"
    );
    let refused = fx.requeue(&foreign).unwrap();
    assert_eq!(refused["outcome"], "refused");
    assert_eq!(fx.status(&foreign), "blocked");
}

#[test]
fn each_episode_is_triaged_once_and_a_human_touch_excludes_it() {
    let fx = fixture();
    let task = fx.task(&[]);
    fx.escalate(&task);
    assert_eq!(fx.qualifying(), vec![task.clone()]);
    fx.triage(json!({"action": "report", "id": task, "blocker": "login", "reason": "sign in"}))
        .unwrap();
    assert!(fx.qualifying().is_empty());
    assert_eq!(fx.excluded_why(&task), "triaged");

    // A new episode qualifies afresh.
    fx.update(json!({"id": task, "status": "backlog"}));
    fx.escalate(&task);
    assert_eq!(fx.qualifying(), vec![task.clone()]);

    // A human comment after the block excludes the episode.
    fx.human()
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "comment": "I am looking at this"}),
        )
        .unwrap();
    assert!(fx.qualifying().is_empty());
    assert_eq!(fx.excluded_why(&task), "human_intervened");
    let error = fx.requeue(&task).unwrap_err();
    assert!(error.contains("nothing was recorded"), "{error}");
    assert_eq!(fx.status(&task), "blocked");

    // A block a human set is the human's own status change.
    let manual = fx.task(&[]);
    fx.update(json!({"id": manual, "status": "in_progress"}));
    fx.human()
        .run_tool(
            "orbit.task.update",
            json!({"id": manual, "status": "blocked", "note": "waiting on design"}),
        )
        .unwrap();
    let late = fx
        .runtime
        .blocked_triage_view(Utc::now() + Duration::hours(73))
        .unwrap();
    let view = late.iter().find(|view| view.task.id == manual).unwrap();
    assert_eq!(view.candidacy.as_str(), "human_intervened");
}

/// The seeded routine, enabled, with its seed-time placeholders rendered.
fn seeded_routine() -> orbit_types::workflow::RoutineDefinition {
    let yaml = include_str!("../../assets/routines/blocked_task_triage.yaml")
        .replace("__ORBIT_ROUTINE_NAME__", "blocked-task-triage-fixture")
        .replace("__ORBIT_OWNER_MACHINE__", "fixture-machine")
        .replace("__ORBIT_BASE_BRANCH__", "main")
        .replace("enabled: false", "enabled: true");
    orbit_common::protocol::yaml::parse_routine_yaml(&yaml).unwrap()
}

#[test]
fn the_block_escalated_trigger_admits_each_escalated_episode_once() {
    let fx = fixture();
    // The trigger observes the workspace's branch.
    let git = |args: &[&str]| {
        let status = crate::git_test_support::command()
            .args([
                "-c",
                "user.name=Orbit Test",
                "-c",
                "user.email=orbit-test@example.com",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&fx.repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(fx.repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "seed"]);
    let runtime = OrbitRuntime::from_roots(&fx.global, &fx.repo.join(".orbit"))
        .unwrap()
        .with_automation_machine_identity(Some("fixture-machine".into()));
    let routine = seeded_routine();
    let pending = |now| {
        evaluate_routine(&runtime, &routine, false, now)
            .unwrap()
            .state
            .unwrap()
            .members
            .unwrap()
            .pending
    };

    let escalated = fx.task(&[]);
    fx.escalate(&escalated);
    let undecided = fx.task(&[]);
    fx.update(json!({"id": undecided, "status": "in_progress"}));
    fx.update(json!({"id": undecided, "status": "blocked", "note": "worker exited"}));
    let touched = fx.task(&[]);
    fx.escalate(&touched);
    fx.human()
        .run_tool(
            "orbit.task.update",
            json!({"id": touched, "comment": "I am looking at this"}),
        )
        .unwrap();
    let triaged = fx.task(&[]);
    fx.escalate(&triaged);
    fx.triage(json!({"action": "report", "id": triaged, "blocker": "login", "reason": "sign in"}))
        .unwrap();

    let now = Utc::now();
    let observed = evaluate_routine(&runtime, &routine, false, now).unwrap();
    assert_eq!(observed.reason, "debouncing", "{observed:?}");
    let members = pending(now);
    assert_eq!(
        members.keys().collect::<Vec<_>>(),
        vec![&escalated],
        "only the untouched, untriaged escalated episode is observed"
    );
    let episode = members[&escalated].fingerprint.clone();
    let due = evaluate_routine(&runtime, &routine, true, now + Duration::minutes(3)).unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 1, "{due:?}");
    assert_eq!(due.batch[0].task_ids, std::slice::from_ref(&escalated));

    // Triaging the episode takes it out of observation; a fresh block on the
    // same task is a new episode.
    fx.triage(
        json!({"action": "report", "id": escalated, "blocker": "login", "reason": "sign in"}),
    )
    .unwrap();
    assert!(pending(now + Duration::minutes(4)).is_empty());
    fx.update(json!({"id": escalated, "status": "backlog"}));
    fx.escalate(&escalated);
    let fresh = pending(Utc::now() + Duration::minutes(5));
    assert_ne!(fresh[&escalated].fingerprint, episode, "a new episode");
}

#[test]
fn a_task_a_live_triage_run_holds_is_decided_only_with_its_inspection_proof() {
    let fx = fixture();
    let git = |args: &[&str]| -> String {
        let output = crate::git_test_support::command()
            .args(args)
            .current_dir(&fx.repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Orbit Test"]);
    git(&["config", "user.email", "orbit-test@example.com"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(fx.repo.join("README.md"), "fixture\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-m", "seed"]);
    let revision = git(&["rev-parse", "HEAD"]);
    let tree = git(&["rev-parse", "HEAD^{tree}"]);

    let task = fx.task(&[]);
    fx.escalate(&task);
    fx.hold_in_triage_run(&task);
    let decide = |action: &str, proof: Value| {
        let mut input = json!({
            "action": action,
            "id": task,
            "blocker": "the worker crashed while the host was rebooting",
            "reason": "the host is back and a later run on it succeeded",
            "options": [
                {"summary": "Requeue", "consequences": "it runs again", "replacement": "- Recovered."},
                {"summary": "Archive", "consequences": "nobody recovers it", "replacement": "- Recovered."},
            ],
            "recommended": 1,
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(proof.as_object().unwrap().clone());
        fx.triage(input)
    };
    // The run's triager owes the proof on every decision: one without it,
    // or with a pair it did not read from the pinned checkout, records,
    // proposes and requeues nothing.
    for action in ["report", "propose", "requeue"] {
        for proof in [
            json!({}),
            json!({"source_revision": revision}),
            json!({
                "source_revision": revision, "observed_revision": revision,
                "observed_tree": "0".repeat(40),
            }),
        ] {
            decide(action, proof.clone()).expect_err(&format!("{action} with {proof} is refused"));
            assert!(fx.triage_comments(&task).is_empty(), "nothing was recorded");
            assert_eq!(fx.status(&task), "blocked", "nothing was requeued");
        }
    }

    // The pair read from the checkout, in whatever hex case Git printed it,
    // records the decision.
    let reported = decide(
        "report",
        json!({
            "source_revision": revision.to_ascii_uppercase(),
            "observed_revision": revision.to_ascii_uppercase(),
            "observed_tree": tree.to_ascii_uppercase(),
        }),
    )
    .unwrap();
    assert_eq!(reported["outcome"], "reported");
    assert_eq!(fx.triage_comments(&task).len(), 1);
}

#[test]
fn a_non_git_triage_run_pins_no_revision_and_its_decisions_report_none() {
    let fx = fixture();
    let task = fx.task(&[]);
    fx.escalate(&task);
    fx.hold_in_triage_run(&task);
    let report = |proof: Value| {
        let mut input = json!({
            "action": "report", "id": task,
            "blocker": "the worker crashed while the host was rebooting",
            "reason": "the operator checks the host",
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(proof.as_object().unwrap().clone());
        fx.triage(input)
    };
    let stale = "0".repeat(40);
    report(json!({"source_revision": null, "observed_revision": stale, "observed_tree": stale}))
        .expect_err("a non-Git checkout has no HEAD to observe");
    assert!(fx.triage_comments(&task).is_empty(), "nothing was recorded");
    let reported = report(json!({"source_revision": null})).unwrap();
    assert_eq!(reported["outcome"], "reported");
    assert_eq!(fx.triage_comments(&task).len(), 1);
}

fn decision_requests(fx: &Fixture, task: &str) -> Vec<orbit_types::task::DecisionRequest> {
    fx.runtime.get_task_decision_requests(task).unwrap()
}

fn propose(fx: &Fixture, task: &str) -> Value {
    fx.triage(json!({
        "action": "propose",
        "id": task,
        "blocker": "criterion 2 contradicts the retired API",
        "options": [
            {"summary": "Drop criterion 2", "consequences": "the old API stays retired", "replacement": "- Recovered."},
            {"summary": "Keep the API", "consequences": "the retirement is reverted", "replacement": "- Recovered.\n- The API answers."},
        ],
        "recommended": 1,
    }))
    .unwrap()
}

#[test]
fn an_enabled_proposal_raises_one_open_decision_request() {
    let fx = fixture_with_config("[decision_channel]\nenabled = true\n");
    let task = fx.task(&[]);
    fx.escalate(&task);
    let proposal = propose(&fx, &task);
    assert_eq!(proposal["disposition"], "decision");
    let requests = decision_requests(&fx, &task);
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = &requests[0];
    assert_eq!(request.state, orbit_types::task::DecisionRequestState::Open);
    assert_eq!(request.gate, orbit_types::task::DecisionGate::BlockTriage);
    assert_eq!(request.task_id, task);
    assert_eq!(
        request
            .answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect::<Vec<_>>(),
        vec!["1", "2", "dismiss"]
    );
    assert_eq!(request.answers[0].label, "Drop criterion 2");
    assert!(
        matches!(&request.binding, orbit_types::task::DecisionBinding::BlockEpisode { episode } if episode == proposal["episode"].as_str().unwrap()),
        "{request:?}"
    );
    assert!(request.id.starts_with("dr-"), "{request:?}");
    assert!(request.expires_at > request.raised_at, "{request:?}");
}

#[test]
fn a_disabled_or_absent_channel_writes_no_decision_request() {
    for config in ["", "[decision_channel]\nenabled = false\n"] {
        let fx = fixture_with_config(config);
        let task = fx.task(&[]);
        fx.escalate(&task);
        propose(&fx, &task);
        assert!(
            decision_requests(&fx, &task).is_empty(),
            "config {config:?} must write no decision request"
        );
        let history = fx.runtime.get_task_history(&task).unwrap();
        assert!(
            history
                .iter()
                .all(|entry| entry.event != orbit_types::task::DECISION_REQUESTED_EVENT),
            "config {config:?} must write no decision_requested entry"
        );
    }
}

#[test]
fn leaving_a_block_episode_supersedes_its_request_without_a_new_proposal() {
    let fx = fixture_with_config("[decision_channel]\nenabled = true\n");
    let task = fx.task(&[]);
    fx.escalate(&task);
    propose(&fx, &task);
    fx.human()
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "status": "backlog"}),
        )
        .unwrap();
    for new_episode in [false, true] {
        if new_episode {
            fx.escalate(&task);
        }
        let requests = decision_requests(&fx, &task);
        assert_eq!(requests.len(), 1, "no replacement request was raised");
        assert_eq!(
            requests[0].state,
            orbit_types::task::DecisionRequestState::Superseded
        );
        let history = fx.runtime.get_task_history(&task).unwrap();
        assert!(
            orbit_types::task::open_decision_requests(
                &fx.runtime.get_task(&task).unwrap(),
                &history,
                None,
                Utc::now()
            )
            .is_empty()
        );
    }
}

#[test]
fn a_second_proposal_supersedes_the_first() {
    let fx = fixture_with_config("[decision_channel]\nenabled = true\n");
    let task = fx.task(&[]);
    fx.escalate(&task);
    propose(&fx, &task);
    fx.update(json!({"id": task, "status": "backlog"}));
    fx.escalate(&task);
    propose(&fx, &task);
    let requests = decision_requests(&fx, &task);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        requests[0].state,
        orbit_types::task::DecisionRequestState::Superseded
    );
    assert_eq!(
        requests[1].state,
        orbit_types::task::DecisionRequestState::Open
    );
    assert_ne!(requests[0].binding.key(), requests[1].binding.key());
    let history = fx.runtime.get_task_history(&task).unwrap();
    assert_eq!(
        orbit_types::task::open_decision_requests(
            &fx.runtime.get_task(&task).unwrap(),
            &history,
            None,
            Utc::now()
        )
        .len(),
        1,
        "at most one request per task is open"
    );
}

#[test]
fn a_pilot_warning_requeue_is_refused_as_a_requeue_whatever_proof_it_carries() {
    let fx = fixture();
    let task = fx.task(&[]);
    let requeue = |proof: Value| {
        let mut input = json!({
            "action": "requeue", "subject": "pilot_warning", "id": task,
            "reason": "the finding is wrong",
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(proof.as_object().unwrap().clone());
        fx.triage(input)
            .expect_err("pilot warning triage never requeues")
    };
    let stale = "0".repeat(40);
    assert_eq!(
        requeue(json!({
            "source_revision": stale, "observed_revision": stale, "observed_tree": stale,
        })),
        requeue(json!({})),
        "an unsupported action is refused before its proof is read"
    );
}
