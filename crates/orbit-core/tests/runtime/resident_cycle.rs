//! The resident cycle: the `resident_due` trigger fires a mandate only when
//! something it watches changed (or its heartbeat elapsed) and its cadence
//! floor allows, never while a cycle is live; core scopes a cycle's tool
//! calls to its grant; an open maintenance notice makes the cycle
//! observe-only and its closing wakes the mandate; a killed cycle leaves a
//! synthesized `incomplete` checkpoint and its recovery never repeats an
//! action the dead cycle completed.

mod assignment_lifecycle;
mod control_admission;
mod landing;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::automation::evaluate_routine;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_store::contracts::{JobRunStoreBackend, KeyedJobRunAdmission};
use orbit_types::task::{TaskArtifact, TaskStatus};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{JobRunState, ResidentComment, RoutineDefinition};
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.other]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";

const TOOL_ENV: [&str; 8] = [
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_RUN_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_ACTIVITY_ID",
    "ORBIT_STEP_INDEX",
];

struct Fixture {
    /// Holds the process environment's lock for the whole test with no
    /// managed-run envelope, so a parallel test's envelope never leaks in.
    env: std::cell::RefCell<Option<orbit_common::test_env::ScopedEnv>>,
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn fixture() -> Fixture {
    let env = std::cell::RefCell::new(Some(orbit_common::test_env::unset(TOOL_ENV)));
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(repo.join(".orbit/config.toml"), CONFIG).unwrap();
    let git = |args: &[&str]| {
        let status = crate::git_test_support::command()
            .args([
                "-c",
                "user.name=Orbit Test",
                "-c",
                "user.email=orbit-test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "seed"]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    Fixture {
        env,
        _root: root,
        global,
        repo,
        runtime,
        jobs,
    }
}

fn session(capability: McpCapability) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [capability].into(),
        ..Default::default()
    }
}

fn call(
    runtime: &OrbitRuntime,
    tool: &str,
    input: Value,
    capability: McpCapability,
) -> Result<Value, String> {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            tool,
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session(capability),
        )
        .map(|outcome| outcome.value)
        .map_err(|error| error.to_string())
}

impl Fixture {
    /// Run `body` as a managed activity of `run_id`: the envelope Orbit
    /// stamps into an agent it spawns, the only source of a call's run binding.
    fn as_cycle_agent<T>(&self, run_id: &str, body: impl FnOnce() -> T) -> T {
        self.env.borrow_mut().take();
        let managed = orbit_common::test_env::scoped(TOOL_ENV.into_iter().map(|name| match name {
            "ORBIT_MANAGED_RUN_CONTEXT" => (name, Some("1")),
            "ORBIT_RUN_ID" => (name, Some(run_id)),
            _ => (name, None),
        }));
        let result = body();
        drop(managed);
        *self.env.borrow_mut() = Some(orbit_common::test_env::unset(TOOL_ENV));
        result
    }

    fn automation_runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some("fixture-machine".into()))
    }

    fn task(&self, title: &str) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    }

    /// A tagged, in-progress assignment watching `watched`, floor 10 minutes.
    fn mandate(&self, watched: &str) -> String {
        let id = self
            .runtime
            .add_task(TaskAddParams {
                title: "[orchestration]: scribe v2 - sol".into(),
                tags: vec!["resident".into()],
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        let body = json!({
            "schema": "resident.mandate/v1",
            "goal": "Deliver the remaining scribe v2 slices",
            "exit_criteria": ["Every scribe-v2 task is done or rejected"],
            "workspaces": [self.runtime.workspace_id().unwrap()],
            "watch": {"tasks": [watched], "tags": ["scribe-v2"]},
            "crew": "sol",
            "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
        });
        self.runtime
            .update_task_as_human(
                &id,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        "resident/mandate.json",
                        body.to_string(),
                    )],
                    ..Default::default()
                },
                "human:daniel".into(),
            )
            .unwrap();
        id
    }

    fn grant(&self, mandate: &str, rights: &[&str]) {
        call(
            &self.runtime,
            "orbit.resident.grant",
            json!({"task_id": mandate, "add": rights,
                   "workspaces": [self.runtime.workspace_id().unwrap()]}),
            McpCapability::Operator,
        )
        .unwrap();
    }

    /// A pending `resident_cycle_pipeline` run for `mandate`, as the routine
    /// admits it.
    fn cycle_run(&self, mandate: &str, key: &str) -> String {
        let KeyedJobRunAdmission::Admitted(run) = self
            .jobs
            .insert_automation_job_run(
                "resident_cycle_pipeline",
                json!({"task_ids": [mandate]}),
                key,
            )
            .unwrap()
        else {
            panic!("a fresh run key admits its run");
        };
        run.run_id
    }

    /// Finish `run_id` with a conclusively stopped owner, as the
    /// decision-request fixtures do.
    fn stop_run(&self, run_id: &str, state: JobRunState) {
        let mut owner = crate::git_test_support::command()
            .args(["hash-object", "--stdin"])
            .current_dir(&self.repo)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        self.jobs
            .mark_job_run_running(run_id, Utc::now(), owner.id())
            .unwrap();
        drop(owner.stdin.take());
        assert!(owner.wait().unwrap().success());
        self.jobs
            .finalize_job_run(run_id, state, Utc::now(), None)
            .unwrap();
    }

    /// The agent's checkpoint for the cycle `brief` started.
    fn checkpoint(&self, brief: &Value, run_id: &str) {
        self.checkpoint_with_outcome(brief, run_id, "idle");
    }

    /// The agent's checkpoint naming `outcome`.
    fn checkpoint_with_outcome(&self, brief: &Value, run_id: &str, outcome: &str) {
        let mandate = brief["task_id"].as_str().unwrap();
        self.runtime
            .append_resident_checkpoint(
                mandate,
                json!({
                    "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                    "cycle": brief["cycle"], "run_id": run_id, "crew": "sol",
                    "recovery_of": brief["recovery_of"], "wake": brief["wake"],
                    "authority": {"rights": brief["authority"]["rights"],
                                  "observe_only": brief["authority"]["observe_only"],
                                  "narrowed_by": brief["authority"]["narrowed_by"]},
                    "actions": [], "decisions": [], "requests": [], "costs": [],
                    "notes": "Nothing to do yet.",
                    "next_wake": {"reason": "heartbeat", "not_before": Utc::now() + Duration::hours(2)},
                    "continuity": "rebuilt", "outcome": outcome, "probe": false
                }),
                Some("codex".into()),
                None,
            )
            .unwrap();
    }

    /// The agent's checkpoint written exactly as `resident_cycle.yaml`
    /// instructs: `continuity` and `conversation_ref` are copied verbatim
    /// from the brief, never a fresh or hand-chosen reference.
    fn checkpoint_as_instructed(&self, brief: &Value, run_id: &str) -> Value {
        let mandate = brief["task_id"].as_str().unwrap();
        self.runtime
            .append_resident_checkpoint(
                mandate,
                json!({
                    "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                    "cycle": brief["cycle"], "run_id": run_id,
                    "crew": brief["crew"],
                    "recovery_of": brief["recovery_of"], "wake": brief["wake"],
                    "authority": {"rights": brief["authority"]["rights"],
                                  "observe_only": brief["authority"]["observe_only"],
                                  "narrowed_by": brief["authority"]["narrowed_by"]},
                    "actions": [], "decisions": [], "requests": [], "costs": [],
                    "notes": "Checkpoint written as instructed.",
                    "next_wake": {"reason": "heartbeat", "not_before": Utc::now() + Duration::hours(2)},
                    "continuity": brief["continuity"],
                    "conversation_ref": brief["conversation_ref"],
                    "outcome": "idle", "probe": brief["mandate"]["probe"]
                }),
                Some("codex".into()),
                None,
            )
            .unwrap()
    }

    /// One complete cycle: observe, the agent's checkpoint, finalize.
    fn complete_cycle(&self, mandate: &str, key: &str) -> Value {
        let run = self.cycle_run(mandate, key);
        let brief = self.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
        assert_eq!(brief["cycle_count"], 1, "{brief}");
        self.checkpoint(&brief, &run);
        let finished = self.runtime.finish_resident_cycle(&run).unwrap();
        assert_eq!(finished["synthesized"], false, "{finished}");
        self.stop_run(&run, JobRunState::Success);
        brief
    }

    /// A clock-tick inspection of the routine at `now`: whether it would
    /// start a cycle run.
    /// The mandates a clock tick at `now` would admit or hold as due.
    fn due_mandates(&self, now: DateTime<Utc>) -> Vec<String> {
        let due = evaluate_routine(&self.automation_runtime(), &routine(), true, now).unwrap();
        due.state
            .and_then(|state| state.members)
            .map(|members| members.pending.into_keys().collect())
            .unwrap_or_default()
    }

    fn would_fire(&self, now: DateTime<Utc>) -> bool {
        evaluate_routine(&self.automation_runtime(), &routine(), true, now)
            .unwrap()
            .reason
            == "would_fire"
    }
}

fn routine() -> RoutineDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 1, "name": "resident-cycle-fixture", "enabled": true,
        "target": "job:resident_cycle_pipeline",
        "trigger": {"state": {
            "kind": "resident_due", "owner_machine": "fixture-machine", "branch": "main",
            "debounce_minutes": 1, "max_wait_minutes": 5, "max_items": 50,
            "batch_size": 1, "retries": 2, "deadline_minutes": 60,
        }},
    }))
    .unwrap()
}

#[test]
fn a_mandate_fires_only_on_change_after_its_floor_and_never_beside_a_live_cycle() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_mandate_fires_only_on_change_after_its_floor_and_never_beside_a_live_cycle",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    let now = Utc::now();
    assert!(
        fx.would_fire(now + Duration::minutes(2)),
        "a mandate that never cycled is due"
    );

    fx.complete_cycle(&mandate, "resident-cycle-1");

    // Nothing it watches changed and the heartbeat is two hours away.
    for minutes in [1, 11, 60] {
        assert!(
            !fx.would_fire(now + Duration::minutes(minutes)),
            "an unchanged mandate starts no run at +{minutes}m"
        );
    }

    // A watched task changes: held by the 10-minute floor, then due.
    fx.runtime
        .update_task_as_human(
            &watched,
            TaskUpdateParams {
                description: Some("Narrowed to the export path.".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    assert!(
        !fx.would_fire(now + Duration::minutes(5)),
        "the floor holds it"
    );
    let due = evaluate_routine(
        &fx.automation_runtime(),
        &routine(),
        true,
        now + Duration::minutes(11),
    )
    .unwrap();
    assert_eq!(
        due.reason, "would_fire",
        "due once the floor allows: {due:?}"
    );

    // An operator comment on the assignment is a wake event too.
    let quiet = fx.mandate(&watched);
    fx.complete_cycle(&quiet, "resident-cycle-quiet");
    let later = now + Duration::minutes(11);
    assert!(
        !fx.due_mandates(later).contains(&quiet),
        "nothing changed yet"
    );
    fx.runtime
        .update_task_as_human(
            &quiet,
            TaskUpdateParams {
                comment: Some("Hold slice 5 until Monday.".into()),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    assert!(
        fx.due_mandates(later).contains(&quiet),
        "the comment wakes it"
    );

    // A decision request on a watched task: the cycle that sees it raised
    // is not woken again until the human answers it.
    let asked = fx
        .runtime
        .add_task(TaskAddParams {
            title: "Choose the export format".into(),
            description: "Daniel decides the export format.".into(),
            acceptance_criteria: vec!["A format is chosen".into()],
            crew: Some("dk".into()),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let asking = fx.mandate(&asked);
    let request = fx
        .runtime
        .run_tool(
            "orbit.decision.raise",
            json!({"task_id": asked, "model": "codex",
            "question": "Which export format ships first?",
            "options": [
                {"label": "CSV", "consequence": "The export writes CSV.", "recommended": true},
                {"label": "JSON", "consequence": "The export writes JSON."},
            ]}),
        )
        .unwrap();
    let request_id = request["id"]
        .as_str()
        .unwrap_or_else(|| panic!("raised request id: {request}"))
        .to_string();
    fx.complete_cycle(&asking, "resident-cycle-asking");
    assert!(
        !fx.due_mandates(later).contains(&asking),
        "an open request it already saw does not wake it"
    );
    fx.runtime
        .answer_decision_request_as_human(
            &request_id,
            "1",
            None,
            "human:daniel".into(),
            Some("cli".into()),
        )
        .unwrap();
    assert!(
        fx.due_mandates(later).contains(&asking),
        "the human's answer wakes it"
    );

    // While one cycle is live, a second run for the mandate starts none.
    let first = fx.cycle_run(&mandate, "resident-cycle-2");
    let started = fx
        .runtime
        .begin_resident_cycle(&first, now + Duration::minutes(11))
        .unwrap();
    assert_eq!(started["cycle_count"], 1, "{started}");
    let second = fx.cycle_run(&mandate, "resident-cycle-3");
    let refused = fx
        .runtime
        .begin_resident_cycle(&second, now + Duration::minutes(11))
        .unwrap();
    assert_eq!(refused["cycle_count"], 0, "{refused}");
    assert_eq!(refused["skip_reason"], "cycle_live");
    assert!(
        !fx.due_mandates(now + Duration::minutes(12))
            .contains(&mandate),
        "a mandate with a live cycle is never due"
    );
}

#[test]
fn a_cycle_reaches_only_its_granted_rights_and_workspaces() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_cycle_reaches_only_its_granted_rights_and_workspaces",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-1");
    let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    assert_eq!(brief["authority"]["rights"], json!(["dispatch"]), "{brief}");

    fx.as_cycle_agent(&run, || {
        // No merge right: completing a task is refused before it runs.
        let error = call(
            &fx.runtime,
            "orbit.task.update",
            json!({"id": watched, "status": "done", "model": "codex"}),
            McpCapability::Agent,
        )
        .expect_err("a cycle without merge cannot land");
        assert!(error.contains("resident_right_refused"), "{error}");
        assert!(error.contains("merge"), "{error}");

        // A workspace outside the mandate's authority is refused by core.
        let error = call(
            &fx.runtime,
            "orbit.task.add",
            json!({"title": "Elsewhere", "description": "Out of scope.",
                   "acceptance_criteria": ["Done."], "complexity": "low", "type": "chore",
                   "workspace": "ws_not_granted", "model": "codex"}),
            McpCapability::Agent,
        )
        .expect_err("a foreign workspace is refused");
        assert!(error.contains("resident_scope_refused"), "{error}");

        // A comment needs no right; its key comes back for the checkpoint.
        let commented = call(
            &fx.runtime,
            "orbit.task.update",
            json!({"id": watched, "comment": "Checked CI.", "model": "codex"}),
            McpCapability::Agent,
        )
        .unwrap();
        let cycle = brief["cycle"].as_str().unwrap();
        assert_eq!(
            commented["resident_action_key"],
            format!("resident:{mandate}:{cycle}:1")
        );
    });

    // Outside its run binding, the same caller is not a resident cycle.
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.as_cycle_agent(&run, || {
        let error = call(
            &fx.runtime,
            "orbit.task.update",
            json!({"id": watched, "comment": "Late.", "model": "codex"}),
            McpCapability::Agent,
        )
        .expect_err("a finished cycle may only read");
        assert!(error.contains("resident_cycle_stale"), "{error}");
    });
}

/// The run's recorded state and step errors, then the tail of its worker log,
/// where the worker's own stderr lands. The run record does not show that log.
fn leaf_diagnostics(fx: &Fixture, run_id: &str) -> String {
    let record = match fx
        .runtime
        .list_job_runs_observed(orbit_core::application::job::JobRunListParams::default())
    {
        Ok(runs) => match runs.into_iter().find(|run| run.run_id == run_id) {
            Some(run) => {
                let steps = run
                    .steps
                    .iter()
                    .map(|step| {
                        format!(
                            "step {} {:?} exit_code={:?} error_code={:?} error_message={:?}",
                            step.step_index,
                            step.state,
                            step.exit_code,
                            step.error_code,
                            step.error_message,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("run state {:?}; steps: [{steps}]", run.state)
            }
            None => "run record not found".to_string(),
        },
        Err(error) => format!("run record unreadable: {error}"),
    };
    let log = fx
        .runtime
        .paths()
        .logs_dir
        .join(format!("{run_id}.worker.log"));
    let tail = match std::fs::read_to_string(&log) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let from = lines.len().saturating_sub(20);
            orbit_common::observability::logging::redact_event_text(&lines[from..].join("\n"))
        }
        Err(error) => format!("no worker log at {}: {error}", log.display()),
    };
    format!("{record}; worker log tail: {tail}")
}

/// A resident's `dispatch` right starts a leaf run through the tool the cycle
/// reference names (ORB-21968): it used to be refused as `capability_denied`
/// whatever the grant. The grant still decides: no right, a foreign workspace,
/// a lapsed cycle and a managed run that is no cycle start nothing. The worker
/// is its own run, so it does not inherit the cycle's run binding.
#[test]
fn a_cycle_with_dispatch_ships_a_leaf_run_and_every_other_caller_is_refused() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_cycle_with_dispatch_ships_a_leaf_run_and_every_other_caller_is_refused",
    ) {
        return;
    }
    let fx = fixture();
    orbit_core::bootstrap::init::init_workspace_at_root(
        &fx.global,
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    // The capture must not be named `*.env` or `*.env.*`: the shipped policy
    // denies read and modify of those, so under a managed sandbox such a write
    // is refused. The worker writes only the two binding fields the assertions
    // below check, after a start sentinel, so a worker that ran is told apart
    // from one that never did.
    const WORKER_BINDING_CAPTURE: &str = "{ echo worker-started; if [ -n \"${ORBIT_MANAGED_RUN_CONTEXT+x}\" ]; then echo \"ORBIT_MANAGED_RUN_CONTEXT=$ORBIT_MANAGED_RUN_CONTEXT\"; fi; if [ -n \"${ORBIT_RUN_ID+x}\" ]; then echo \"ORBIT_RUN_ID=$ORBIT_RUN_ID\"; fi; } > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"";
    let worker_binding = fx._root.path().join("worker-binding.txt");
    orbit_core::test_support::install_substitute_pipeline_worker([
        "sh".to_string(),
        "-c".to_string(),
        WORKER_BINDING_CAPTURE.to_string(),
        worker_binding.to_string_lossy().into_owned(),
    ]);
    let leaf = |title: &str| {
        fx.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    let workspace = fx.runtime.workspace_id().unwrap();
    let ship = |task: &str, workspace: &str| {
        call(
            &fx.runtime,
            "orbit.workflow.ship",
            json!({"task_ids": [task], "mode": "local", "workspace": workspace, "model": "codex"}),
            McpCapability::Agent,
        )
    };
    let leaf_runs = || {
        fx.runtime
            .list_job_runs(orbit_core::application::job::JobRunListParams::default())
            .unwrap()
            .into_iter()
            .filter(|run| run.job_id != "resident_cycle_pipeline")
            .map(|run| run.run_id)
            .collect::<Vec<_>>()
    };

    let task = leaf("Leaf");
    let mandate = fx.mandate(&task);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-ship");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    let shipped = fx.as_cycle_agent(&run, || {
        let error = ship(&task, "ws_not_granted").expect_err("a foreign workspace is refused");
        assert!(error.contains("resident_scope_refused"), "{error}");
        assert!(leaf_runs().is_empty(), "a refused ship starts nothing");
        ship(&task, &workspace).expect("a live dispatch grant starts the leaf run")
    });
    let shipped_run = shipped["run_id"].as_str().expect("the leaf's run id");
    assert_eq!(leaf_runs(), [shipped_run], "{shipped}");
    assert!(
        shipped["resident_action_key"]
            .as_str()
            .is_some_and(|key| key.starts_with(&format!("resident:{mandate}:"))),
        "the write is on the cycle's ledger: {shipped}"
    );

    // The state is read before the capture is checked: a terminal run has
    // already exited, so any capture its worker wrote is on disk by then. A run
    // that ends without one fails at once, with its own record and log tail.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let finished = fx
            .runtime
            .show_job_run_observed(shipped_run)
            .is_ok_and(|run| run.state.is_terminal());
        if worker_binding.exists() {
            break;
        }
        if finished || std::time::Instant::now() >= deadline {
            panic!(
                "the leaf's worker wrote no binding capture (run {shipped_run}, finished: {finished}): {}",
                leaf_diagnostics(&fx, shipped_run)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let inherited = std::fs::read_to_string(&worker_binding).unwrap();
    assert!(
        inherited.lines().any(|line| line == "worker-started"),
        "the binding capture lacks the worker's start sentinel"
    );
    for name in ["ORBIT_MANAGED_RUN_CONTEXT=", "ORBIT_RUN_ID="] {
        assert!(
            !inherited.lines().any(|line| line.starts_with(name)),
            "a leaf worker judged as the cycle that started it loses its own tool calls to \
             the cycle guard: {name}"
        );
    }

    // After its cycle, the same run is no longer dispatching anything.
    let later = leaf("After the cycle");
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.as_cycle_agent(&run, || {
        let error = ship(&later, &workspace).expect_err("a lapsed cycle may only read");
        assert!(error.contains("resident_cycle_stale"), "{error}");
    });

    // A live cycle without the right.
    let promote_only = fx.mandate(&later);
    fx.grant(&promote_only, &["promote"]);
    let run = fx.cycle_run(&promote_only, "resident-cycle-no-dispatch");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    fx.as_cycle_agent(&run, || {
        let error = ship(&later, &workspace).expect_err("promote does not dispatch");
        assert!(error.contains("resident_right_refused"), "{error}");
        assert!(error.contains("dispatch"), "{error}");
    });

    // Any other managed run stays held to the operator row.
    let KeyedJobRunAdmission::Admitted(other) = fx
        .jobs
        .insert_automation_job_run("agent_explore_pipeline", json!({}), "not-a-cycle")
        .unwrap()
    else {
        panic!("a fresh run key admits its run");
    };
    fx.as_cycle_agent(&other.run_id, || {
        let error = ship(&later, &workspace).expect_err("only a cycle holds a grant");
        assert!(
            error.contains("requires the `operator` capability"),
            "{error}"
        );
    });
    assert_eq!(leaf_runs().len(), 2, "only the granted ship started a leaf");
}

/// Restoring lint is context maintenance, not a resident read (ORB-21580).
#[test]
fn restoring_lint_refuses_resident_cycles_without_partial_context_writes() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::restoring_lint_refuses_resident_cycles_without_partial_context_writes",
    ) {
        return;
    }
    let fx = fixture();
    let named = fx.task("Restore named context");
    let swept = fx.task("Restore swept context");
    std::fs::write(fx.repo.join("pruned.rs"), "pub fn restored() {}\n").unwrap();
    // Seed the exact history produced by the retired pruning path, using the
    // coordinated store just as persisted old task records would carry it.
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&fx.runtime.global_root()),
    )
    .unwrap();
    let backends = orbit_store::compose::workspace_coordinated_backends(
        registry,
        fx.runtime.workspace_id().unwrap(),
        fx.runtime.sqlite_store().unwrap(),
    )
    .unwrap();
    let append_history = |id: &str, event: &str, note: String| {
        backends
            .task
            .history
            .update_task_history(
                id,
                orbit_store::contracts::TaskHistoryUpdateParams {
                    actor: "system".into(),
                    append_history: vec![orbit_types::task::TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: event.into(),
                        note: Some(note),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
    };
    for id in [&named, &swept] {
        append_history(
            id,
            "context_files_pruned",
            "dropped: file:pruned.rs (selector anchor not found in workspace)".into(),
        );
        assert_eq!(
            fx.runtime.plan_context_file_restore(id).unwrap().restored,
            ["file:pruned.rs"]
        );
    }
    let mandate = fx.mandate(&named);
    fx.grant(
        &mandate,
        &["dispatch", "promote", "merge", "plugin_upgrade"],
    );
    let run = fx.cycle_run(&mandate, "resident-restoring-lint");
    let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();

    // The native `orbit task lint` command uses this public registry API.
    // Lint is intentionally inactive on MCP / `orbit tool run`.
    let lint = |input| {
        fx.runtime
            .run_tool("orbit.task.lint", input)
            .map_err(|error| error.to_string())
    };

    for (state, expected) in [
        ("live", "resident_tool_refused"),
        ("out_of_scope", "resident_scope_refused"),
        ("expired", "resident_cycle_stale"),
        ("finished", "resident_cycle_stale"),
    ] {
        if state == "expired" {
            // Advance only persisted lease time, never wall-clock sleeps.
            let live = orbit_types::workflow::ResidentLiveCycle {
                cycle: brief["cycle"].as_str().unwrap().into(),
                run_id: run.clone(),
                expires_at: Utc::now() - Duration::hours(1),
            };
            append_history(
                &mandate,
                "resident_cycle_changed",
                serde_json::to_string(&live).unwrap(),
            );
        } else if state == "finished" {
            fx.runtime.finish_resident_cycle(&run).unwrap();
        }
        let before: Vec<_> = [&named, &swept, &mandate]
            .into_iter()
            .map(|id| {
                (
                    fx.runtime.get_task(id).unwrap().context_files,
                    serde_json::to_value(fx.runtime.get_task_history(id).unwrap()).unwrap(),
                )
            })
            .collect();
        fx.as_cycle_agent(&run, || {
            for id in [Some(&named), None] {
                let mut input = json!({"model": "codex"});
                if let Some(id) = id {
                    input["id"] = json!(id);
                }
                // Ordinary lint remains a read even after the lease expires.
                let read = lint(input.clone()).unwrap();
                if id.is_none() {
                    assert_eq!(read["dry_run"], true);
                    assert_eq!(read["total_restorable"], 2);
                }
                input["restore_pruned"] = json!(true);
                if state == "out_of_scope" {
                    input["workspace"] = json!("ws_not_granted");
                }
                let error = lint(input)
                    .expect_err("restoring lint must not bypass the resident write guard");
                assert!(error.contains(expected), "{state}: {error}");
            }
        });
        for (id, (selectors, history)) in [&named, &swept, &mandate].into_iter().zip(before) {
            assert_eq!(
                fx.runtime.get_task(id).unwrap().context_files,
                selectors,
                "{state}: {id}"
            );
            assert_eq!(
                serde_json::to_value(fx.runtime.get_task_history(id).unwrap()).unwrap(),
                history,
                "{state}: {id}: refusal and reads must not write history or a ledger"
            );
        }
    }
    // Without a resident run binding, both operator entry paths still restore.
    for input in [
        json!({"id": named, "restore_pruned": true}),
        json!({"restore_pruned": true}),
    ] {
        lint(input).unwrap();
    }
    for id in [&named, &swept] {
        assert_eq!(
            fx.runtime.get_task(id).unwrap().context_files,
            ["file:pruned.rs"]
        );
        assert_eq!(
            fx.runtime
                .get_task_history(id)
                .unwrap()
                .iter()
                .filter(|row| row.event == "context_files_restored")
                .count(),
            1
        );
    }
}

#[test]
fn an_open_maintenance_notice_makes_the_cycle_observe_only_and_its_close_wakes_it() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::an_open_maintenance_notice_makes_the_cycle_observe_only_and_its_close_wakes_it",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch", "promote", "merge"]);
    let posted = call(
        &fx.runtime,
        "orbit.notice.post",
        json!({"kind": "maintenance", "title": "Swap disks", "expires_in_hours": 24}),
        McpCapability::Operator,
    )
    .unwrap();
    let notice = posted["notice"]["id"].as_str().unwrap().to_string();

    let now = Utc::now();
    let run = fx.cycle_run(&mandate, "resident-cycle-1");
    let brief = fx.runtime.begin_resident_cycle(&run, now).unwrap();
    assert_eq!(brief["authority"]["observe_only"], true, "{brief}");
    assert_eq!(brief["authority"]["rights"], json!([]));
    assert_eq!(
        brief["authority"]["narrowed_by"],
        format!("notice:{notice}")
    );
    fx.as_cycle_agent(&run, || {
        for (tool, input) in [
            (
                "orbit.task.update",
                json!({"id": watched, "status": "in-progress", "model": "codex"}),
            ),
            (
                "orbit.task.update",
                json!({"id": watched, "status": "backlog", "model": "codex"}),
            ),
            (
                "orbit.task.update",
                json!({"id": watched, "status": "done", "model": "codex"}),
            ),
        ] {
            let error = call(&fx.runtime, tool, input, McpCapability::Agent)
                .expect_err("observe-only refuses every right");
            assert!(error.contains("observe-only"), "{tool}: {error}");
        }
    });
    fx.checkpoint(&brief, &run);
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.stop_run(&run, JobRunState::Success);

    assert!(
        !fx.would_fire(now + Duration::minutes(11)),
        "nothing changed"
    );
    call(
        &fx.runtime,
        "orbit.notice.close",
        json!({"id": notice, "reason": "disks swapped"}),
        McpCapability::Operator,
    )
    .unwrap();
    assert!(
        fx.would_fire(now + Duration::minutes(11)),
        "the close wakes it"
    );
}

#[test]
fn a_killed_cycle_is_recovered_without_repeating_its_completed_actions() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_killed_cycle_is_recovered_without_repeating_its_completed_actions",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    let dead_run = fx.cycle_run(&mandate, "resident-cycle-1");
    let dead = fx
        .runtime
        .begin_resident_cycle(&dead_run, Utc::now())
        .unwrap();
    let comment = json!({"id": watched, "comment": "Checked CI.", "model": "codex"});
    let key = fx.as_cycle_agent(&dead_run, || {
        call(
            &fx.runtime,
            "orbit.task.update",
            comment.clone(),
            McpCapability::Agent,
        )
        .unwrap()["resident_action_key"]
            .as_str()
            .unwrap()
            .to_string()
    });
    // Killed: no checkpoint, no finalize.
    fx.stop_run(&dead_run, JobRunState::Failed);
    let comments = fx.runtime.get_task_comments(&watched).unwrap().len();

    let run = fx.cycle_run(&mandate, "resident-cycle-2");
    let brief = fx
        .runtime
        .begin_resident_cycle(&run, Utc::now() + Duration::minutes(11))
        .unwrap();
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    assert_eq!(brief["recovery_of"], dead["cycle"]);
    let synthesized = fx
        .runtime
        .get_task_comments(&mandate)
        .unwrap()
        .iter()
        .find_map(
            |comment| match ResidentComment::parse(&comment.message).ok()? {
                ResidentComment::Checkpoint(record) if record.cycle == dead["cycle"] => {
                    Some(record)
                }
                _ => None,
            },
        )
        .expect("the dead cycle has a synthesized checkpoint");
    assert_eq!(synthesized.outcome, "incomplete");
    assert!(synthesized.error.is_some());
    assert_eq!(synthesized.actions.len(), 1);
    assert_eq!(synthesized.actions[0].key, key);
    assert_eq!(synthesized.actions[0].result, "ok");

    let retried = fx.as_cycle_agent(&run, || {
        call(
            &fx.runtime,
            "orbit.task.update",
            comment,
            McpCapability::Agent,
        )
        .unwrap()
    });
    assert_eq!(retried["resident_action"], "already_done", "{retried}");
    assert_eq!(retried["key"], key);
    assert_eq!(
        fx.runtime.get_task_comments(&watched).unwrap().len(),
        comments,
        "the recovered action did not run again"
    );
    fx.checkpoint(&brief, &run);
    let finished = fx.runtime.finish_resident_cycle(&run).unwrap();
    assert_eq!(finished["synthesized"], false, "{finished}");
}

#[test]
fn a_status_request_wake_survives_later_wakes_until_the_next_cycle() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_status_request_wake_survives_later_wakes_until_the_next_cycle",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    let now = Utc::now();
    fx.complete_cycle(&mandate, "resident-cycle-status-seed");

    let control = |action: &str, status_request: bool| {
        call(
            &fx.runtime,
            "orbit.resident.control",
            json!({"task_id": mandate.as_str(), "action": action,
                   "reason": "Manager control", "status_request": status_request}),
            McpCapability::Operator,
        )
        .unwrap();
    };
    let admitted_cycle = |key: &str, at: DateTime<Utc>| -> Value {
        let run = fx.cycle_run(&mandate, key);
        let brief = fx.runtime.begin_resident_cycle(&run, at).unwrap();
        assert_eq!(brief["cycle_count"], 1, "{brief}");
        fx.checkpoint(&brief, &run);
        fx.runtime.finish_resident_cycle(&run).unwrap();
        fx.stop_run(&run, JobRunState::Success);
        brief
    };
    let reasons = |brief: &Value| -> Vec<String> {
        brief["wake"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|reason| reason.as_str().map(str::to_string))
            .collect()
    };

    // A plain wake after the status-request wake must not overwrite it.
    control("wake", true);
    control("wake", false);
    let brief = admitted_cycle(
        "resident-cycle-status-plain-wake",
        now + Duration::minutes(11),
    );
    assert_eq!(brief["status_request"], true, "{brief}");
    assert!(
        reasons(&brief).contains(&"status_request".to_string()),
        "{brief}"
    );

    // A pause/resume pair intervening before the cycle must not lose it.
    control("wake", true);
    control("pause", false);
    control("resume", false);
    let brief = admitted_cycle(
        "resident-cycle-status-pause-resume",
        now + Duration::minutes(22),
    );
    assert_eq!(brief["status_request"], true, "{brief}");
    assert!(
        reasons(&brief).contains(&"status_request".to_string()),
        "{brief}"
    );

    // That cycle consumed the request: a later plain wake carries none.
    control("wake", false);
    let brief = admitted_cycle(
        "resident-cycle-status-consumed",
        now + Duration::minutes(33),
    );
    assert_eq!(brief["status_request"], false, "{brief}");
    assert!(
        !reasons(&brief).contains(&"status_request".to_string()),
        "{brief}"
    );
}

#[test]
fn a_stopped_assignment_whose_live_cycle_dies_is_handed_to_review_without_a_new_cycle() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_stopped_assignment_whose_live_cycle_dies_is_handed_to_review_without_a_new_cycle",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    fx.runtime
        .update_task_as_human(
            &mandate,
            TaskUpdateParams {
                plan: Some("Run resident cycles until stopped.".into()),
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    // A cycle is live when the mandate is stopped: the stop defers to its
    // finalizer.
    let run = fx.cycle_run(&mandate, "resident-cycle-stopped");
    let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    let dead_cycle = brief["cycle"].as_str().unwrap().to_string();
    let stopped = call(
        &fx.runtime,
        "orbit.resident.control",
        json!({"task_id": mandate, "action": "stop", "reason": "Work is done for now"}),
        McpCapability::Operator,
    )
    .unwrap();
    assert_eq!(stopped["handoff"], "cycle_live", "{stopped}");
    assert_eq!(
        fx.runtime.get_task(&mandate).unwrap().status,
        TaskStatus::InProgress
    );

    // Killed before finalization: no checkpoint, no finalize.
    fx.stop_run(&run, JobRunState::Failed);

    // The next observe for the mandate starts no agent cycle, but recovers
    // the handoff.
    let recovery_run = fx.cycle_run(&mandate, "resident-cycle-recovery");
    let recovered = fx
        .runtime
        .begin_resident_cycle(&recovery_run, Utc::now() + Duration::minutes(11))
        .unwrap();
    assert_eq!(recovered["cycle_count"], 0, "{recovered}");
    assert_eq!(
        recovered["skip_reason"], "mandate_not_active",
        "{recovered}"
    );
    assert_eq!(recovered["handoff"], "review", "{recovered}");
    assert_eq!(
        fx.runtime.get_task(&mandate).unwrap().status,
        TaskStatus::Review,
        "the stopped assignment reaches review"
    );

    // The dead cycle has its synthesized final checkpoint, and no
    // checkpoint (or cycle) was started for the recovery run.
    let checkpoints: Vec<_> = fx
        .runtime
        .get_task_comments(&mandate)
        .unwrap()
        .iter()
        .filter_map(
            |comment| match ResidentComment::parse(&comment.message).ok()? {
                ResidentComment::Checkpoint(record) => Some(*record),
                _ => None,
            },
        )
        .collect();
    let synthesized = checkpoints
        .iter()
        .find(|checkpoint| checkpoint.cycle == dead_cycle)
        .expect("the dead stopped cycle has a synthesized checkpoint");
    assert_eq!(synthesized.outcome, "incomplete");
    assert!(synthesized.error.is_some());
    assert!(
        checkpoints
            .iter()
            .all(|checkpoint| checkpoint.cycle != format!("rcy-{recovery_run}")),
        "no new agent cycle was started: {checkpoints:?}"
    );
    let shown = fx.runtime.show_resident_mandate(&mandate).unwrap();
    assert!(shown["live_cycle"].is_null(), "{shown}");
}

#[test]
fn a_write_is_refused_when_the_run_lookup_errors_but_a_missing_run_proceeds() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_write_is_refused_when_the_run_lookup_errors_but_a_missing_run_proceeds",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["merge"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-1");
    fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();

    // A run id that does not exist is not a resident cycle: the call
    // proceeds as an ordinary call, as before.
    fx.as_cycle_agent("jrun-no-such-run", || {
        call(
            &fx.runtime,
            "orbit.task.update",
            json!({"id": watched, "comment": "Not a resident run.", "model": "codex"}),
            McpCapability::Agent,
        )
        .unwrap();
    });

    // Corrupt the stored run so its lookup fails with a store error,
    // not `NotFound`: the row no longer parses as a job run.
    let connection = rusqlite::Connection::open(fx.global.join("orbit.db")).unwrap();
    connection
        .execute(
            "UPDATE job_runs SET state = 'not-a-state' WHERE run_id = ?1",
            rusqlite::params![run],
        )
        .unwrap();
    drop(connection);

    let comments_before = fx.runtime.get_task_comments(&watched).unwrap().len();
    fx.as_cycle_agent(&run, || {
        // A write must fail closed rather than bypass the guard.
        let error = call(
            &fx.runtime,
            "orbit.task.update",
            json!({"id": watched, "comment": "Should not land.", "model": "codex"}),
            McpCapability::Agent,
        )
        .expect_err("a write is refused when the run lookup errors");
        assert!(error.contains("resident_lookup_refused"), "{error}");

        // Reads still pass: they change nothing.
        call(
            &fx.runtime,
            "orbit.task.show",
            json!({"id": watched}),
            McpCapability::Agent,
        )
        .unwrap();
    });
    assert_eq!(
        fx.runtime.get_task_comments(&watched).unwrap().len(),
        comments_before,
        "the refused write did not run"
    );
}

#[test]
fn a_transient_run_lookup_error_keeps_a_live_cycle_while_a_missing_or_expired_run_recovers() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_transient_run_lookup_error_keeps_a_live_cycle_while_a_missing_or_expired_run_recovers",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 4");
    let mandate = fx.mandate(&watched);
    let now = Utc::now();

    // A live cycle holds the mandate.
    let live_run = fx.cycle_run(&mandate, "resident-cycle-live");
    let started = fx.runtime.begin_resident_cycle(&live_run, now).unwrap();
    assert_eq!(started["cycle_count"], 1, "{started}");
    let live_cycle = started["cycle"].as_str().unwrap().to_string();

    // Corrupt the live run's row so its lookup fails with a store error,
    // not `NotFound`: the row no longer parses as a job run.
    let db = fx.global.join("orbit.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    let state: String = connection
        .query_row(
            "SELECT state FROM job_runs WHERE run_id = ?1",
            rusqlite::params![live_run],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE job_runs SET state = 'not-a-state' WHERE run_id = ?1",
            rusqlite::params![live_run],
        )
        .unwrap();
    drop(connection);

    // The lookup error surfaces instead of marking the live cycle dead:
    // no takeover, no synthesized checkpoint.
    let takeover_run = fx.cycle_run(&mandate, "resident-cycle-takeover");
    fx.runtime
        .begin_resident_cycle(&takeover_run, now + Duration::minutes(11))
        .expect_err("a lookup error is not a dead cycle");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute(
            "UPDATE job_runs SET state = ?1 WHERE run_id = ?2",
            rusqlite::params![state, live_run],
        )
        .unwrap();
    drop(connection);

    // The cycle survived the error: the same run is still refused as live.
    let refused = fx
        .runtime
        .begin_resident_cycle(&takeover_run, now + Duration::minutes(11))
        .unwrap();
    assert_eq!(refused["cycle_count"], 0, "{refused}");
    assert_eq!(refused["skip_reason"], "cycle_live", "{refused}");

    // A missing run is dead: the next run recovers the cycle.
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute(
            "DELETE FROM job_runs WHERE run_id = ?1",
            rusqlite::params![live_run],
        )
        .unwrap();
    drop(connection);
    let recovered = fx
        .runtime
        .begin_resident_cycle(&takeover_run, now + Duration::minutes(11))
        .unwrap();
    assert_eq!(recovered["cycle_count"], 1, "{recovered}");
    assert_eq!(recovered["recovery_of"], live_cycle, "{recovered}");
    let takeover_cycle = recovered["cycle"].as_str().unwrap().to_string();

    // An expired lease is dead too, without touching the store.
    let expired = fx
        .runtime
        .begin_resident_cycle(
            &fx.cycle_run(&mandate, "resident-cycle-expired"),
            now + Duration::minutes(60),
        )
        .unwrap();
    assert_eq!(expired["cycle_count"], 1, "{expired}");
    assert_eq!(expired["recovery_of"], takeover_cycle, "{expired}");
}

#[test]
fn brief_reconstructs_mandate_authority_checkpoints_requests_wake_and_notices_without_conversation_ref()
 {
    if !super::dispatch_admission::isolated(
        "resident_cycle::brief_reconstructs_mandate_authority_checkpoints_requests_wake_and_notices_without_conversation_ref",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx
        .runtime
        .add_task(TaskAddParams {
            title: "Ship scribe v2 slice 5".into(),
            description: "Slice 5.".into(),
            acceptance_criteria: vec!["Green".into()],
            tags: vec!["scribe-v2".into()],
            crew: Some("dk".into()),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let mandate = fx.mandate(&watched);
    fx.grant(&mandate, &["dispatch", "promote"]);
    let now = Utc::now();

    // Run 6 cycles so the brief carries the newest 5 checkpoints and 1 older checkpoint note.
    for index in 1..=6 {
        let step_time = now + Duration::minutes(index * 15);
        if index > 1 {
            fx.runtime
                .update_task_as_human(
                    &watched,
                    TaskUpdateParams {
                        comment: Some(format!("Progress update {index}")),
                        ..Default::default()
                    },
                    "human:daniel".into(),
                )
                .unwrap();
        }
        let run = fx.cycle_run(&mandate, &format!("resident-brief-cycle-{index}"));
        let brief = fx.runtime.begin_resident_cycle(&run, step_time).unwrap();
        assert_eq!(brief["cycle_count"], 1, "{brief}");
        fx.runtime
            .append_resident_checkpoint(
                &mandate,
                json!({
                    "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                    "cycle": brief["cycle"], "run_id": run, "crew": "sol",
                    "recovery_of": brief["recovery_of"], "wake": brief["wake"],
                    "authority": {"rights": brief["authority"]["rights"],
                                  "observe_only": brief["authority"]["observe_only"],
                                  "narrowed_by": brief["authority"]["narrowed_by"]},
                    "actions": [], "decisions": [], "requests": [], "costs": [],
                    "notes": format!("Note from cycle {index}."),
                    "next_wake": {"reason": "heartbeat", "not_before": step_time + Duration::hours(2)},
                    "continuity": "rebuilt", "outcome": "idle", "probe": false,
                    // Cycle 6 leaves conversation_ref removed (None).
                    "conversation_ref": (index < 6).then(|| format!("conv-{index}")),
                }),
                Some("codex".into()),
                None,
            )
            .unwrap();
        fx.runtime.finish_resident_cycle(&run).unwrap();
        fx.stop_run(&run, JobRunState::Success);
    }

    // Raise two decision requests (one answered by the human, one still open) and post an in-scope notice.
    let answered_req = fx
        .runtime
        .run_tool(
            "orbit.decision.raise",
            json!({
                "task_id": watched,
                "model": "codex",
                "question": "Which serialization format should slice 5 use?",
                "options": [
                    {"label": "JSON", "consequence": "Use JSON payloads.", "recommended": true},
                    {"label": "YAML", "consequence": "Use YAML payloads."}
                ]
            }),
        )
        .unwrap();
    let answered_id = answered_req["id"].as_str().unwrap().to_string();
    fx.runtime
        .answer_decision_request_as_human(
            &answered_id,
            "1",
            Some("JSON matches checkpoint records.".into()),
            "human:daniel".into(),
            Some("cli".into()),
        )
        .unwrap();

    let watched_open = fx
        .runtime
        .add_task(TaskAddParams {
            title: "Decide workspace B widening".into(),
            description: "Slice 5 widening.".into(),
            acceptance_criteria: vec!["Decided".into()],
            tags: vec!["scribe-v2".into()],
            crew: Some("dk".into()),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let open_req = fx
        .runtime
        .run_tool(
            "orbit.decision.raise",
            json!({
                "task_id": watched_open,
                "model": "codex",
                "question": "Should we widen to workspace B?",
                "options": [
                    {"label": "Keep current scope", "consequence": "Stay in workspace A.", "recommended": true},
                    {"label": "Widen", "consequence": "Include workspace B."}
                ]
            }),
        )
        .unwrap();
    let open_id = open_req["id"].as_str().unwrap().to_string();

    let notice_id = call(
        &fx.runtime,
        "orbit.notice.post",
        json!({
            "kind": "info",
            "title": "Registry mirror degraded",
            "body": "Expect slower fetches.",
            "expires_in_hours": 24
        }),
        McpCapability::Operator,
    )
    .unwrap()["notice"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let run7 = fx.cycle_run(&mandate, "resident-brief-cycle-7");
    let brief = fx
        .runtime
        .begin_resident_cycle(&run7, now + Duration::minutes(120))
        .unwrap();
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    assert_eq!(brief["continuity"], "rebuilt");
    assert_eq!(brief["conversation_ref"], Value::Null);
    assert_eq!(
        brief["mandate"]["goal"],
        "Deliver the remaining scribe v2 slices"
    );
    assert_eq!(
        brief["standing_goals"]["exit_criteria"],
        json!(["Every scribe-v2 task is done or rejected"])
    );
    assert_eq!(brief["authority"]["rights"], json!(["dispatch", "promote"]));

    let checkpoints = brief["checkpoints"].as_array().unwrap();
    assert_eq!(
        checkpoints.len(),
        orbit_types::workflow::RESIDENT_BRIEF_CHECKPOINTS
    );
    assert_eq!(checkpoints[0]["notes"], "Note from cycle 6.");
    assert_eq!(checkpoints[4]["notes"], "Note from cycle 2.");
    let older = brief["older_checkpoint_notes"].as_array().unwrap();
    assert_eq!(older.len(), 1);
    assert_eq!(older[0]["notes"], "Note from cycle 1.");

    let requests = brief["open_requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let answered_entry = requests
        .iter()
        .find(|entry| entry["id"] == answered_id)
        .expect("answered request in brief");
    assert_eq!(answered_entry["state"], "answered");
    assert_eq!(answered_entry["answer_id"], "1");
    assert_eq!(answered_entry["answer"], "JSON");
    let open_entry = requests
        .iter()
        .find(|entry| entry["id"] == open_id)
        .expect("open request in brief");
    assert_eq!(open_entry["state"], "open");

    let wake_reasons = brief["wake"]["reasons"].as_array().unwrap();
    assert!(
        wake_reasons.contains(&json!("digest_changed")),
        "{wake_reasons:?}"
    );

    let notices = brief["notices"].as_array().unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["id"], notice_id);
    assert_eq!(notices[0]["untrusted_title"], "Registry mirror degraded");
}

#[cfg(unix)]
#[test]
fn continuity_adapter_resumes_live_reference_and_records_rebuilt_when_reference_expired() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::continuity_adapter_resumes_live_reference_and_records_rebuilt_when_reference_expired",
    ) {
        return;
    }
    use orbit_engine::{V2AuditWriter, V2DispatchInput, dispatch_v2_activity, load_activity_asset};
    use orbit_types::resource::ExecutorResource;
    use orbit_types::workflow::ExecutorDef;
    use std::os::unix::fs::PermissionsExt;

    let fx = fixture();
    let bin_dir = tempfile::tempdir().unwrap();
    let program = bin_dir.path().join("codex");
    let invocations_log = bin_dir.path().join("invocations.jsonl");
    let script = format!(
        r#"#!/bin/sh
stdin_file="$(mktemp)"
cat > "$stdin_file"
python3 - "$stdin_file" '{log}' "$@" <<'PY'
import json, pathlib, sys
stdin_path = pathlib.Path(sys.argv[1])
log_path = pathlib.Path(sys.argv[2])
args = sys.argv[3:]
prompt = stdin_path.read_text()
stdin_path.unlink(missing_ok=True)
envelope = json.loads(prompt.strip().splitlines()[-1])
brief = envelope["input"]["brief"]
with log_path.open("a") as f:
    f.write(json.dumps({{"args": args, "continuity": brief.get("continuity"), "conversation_ref": brief.get("conversation_ref"), "cycle": brief.get("cycle")}}) + "\n")
if "resume" in args:
    idx = args.index("resume")
    if idx + 1 < len(args) and args[idx + 1] == "conv-expired":
        sys.stderr.write("session conv-expired has expired\n")
        sys.exit(1)
print(json.dumps({{"schemaVersion": 1, "status": "success", "result": {{"outcome": "idle", "summary": "ok"}}, "error": None}}))
PY
"#,
        log = invocations_log.display()
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

    let resource: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/codex.yaml")).unwrap();
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    def.sandbox = None;
    fx.runtime.upsert_executor_def(&def).unwrap();

    let watched = fx.task("Ship scribe v2 slice 5");
    let mandate = fx.mandate(&watched);
    let now = Utc::now();

    // Cycle 1: establishes a live conversation_ref ("conv-live").
    let run1 = fx.cycle_run(&mandate, "resident-continuity-1");
    let brief1 = fx.runtime.begin_resident_cycle(&run1, now).unwrap();
    assert_eq!(brief1["continuity"], "rebuilt");
    assert_eq!(brief1["conversation_ref"], Value::Null);
    fx.runtime
        .append_resident_checkpoint(
            &mandate,
            json!({
                "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                "cycle": brief1["cycle"], "run_id": run1, "crew": "sol",
                "recovery_of": null, "wake": brief1["wake"],
                "authority": {"rights": [], "observe_only": false, "narrowed_by": null},
                "actions": [], "decisions": [], "requests": [], "costs": [],
                "notes": "Cycle 1 seeded conv-live.",
                "next_wake": {"reason": "heartbeat", "not_before": now + Duration::hours(2)},
                "continuity": "rebuilt", "outcome": "idle", "probe": false,
                "conversation_ref": "conv-live"
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    fx.runtime.finish_resident_cycle(&run1).unwrap();
    fx.stop_run(&run1, JobRunState::Success);

    // Cycle 2: live reference ("conv-live") -> provider resumes conversation and still receives brief.
    let run2 = fx.cycle_run(&mandate, "resident-continuity-2");
    let brief2 = fx
        .runtime
        .begin_resident_cycle(&run2, now + Duration::minutes(130))
        .unwrap();
    assert_eq!(brief2["continuity"], "resumed");
    assert_eq!(brief2["conversation_ref"], "conv-live");

    let mut activity_asset =
        load_activity_asset(include_str!("../../assets/activities/resident_cycle.yaml")).unwrap();
    if let orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(ref mut loop_spec) =
        activity_asset.spec.spec
    {
        loop_spec.provider = orbit_types::workflow::activity_job::Provider::Codex;
        loop_spec.crew_name = Some("sol".into());
    }
    let audit_dir = tempfile::tempdir().unwrap();
    let audit2 = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_test",
        &run2,
        "codex:test".to_string(),
        None,
    )
    .unwrap();
    let outcome2 = dispatch_v2_activity(V2DispatchInput {
        activity_name: "resident_cycle",
        spec: &activity_asset.spec.spec,
        fs_profile: None,
        input: json!({"brief": brief2, "crew": "sol"}),
        audit: audit2,
        run_id: &run2,
        host: Some(&fx.runtime),
    })
    .unwrap();
    assert!(outcome2.success, "{outcome2:?}");
    let cp2 = fx
        .runtime
        .append_resident_checkpoint(
            &mandate,
            json!({
                "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                "cycle": brief2["cycle"], "run_id": run2, "crew": "sol",
                "recovery_of": null, "wake": brief2["wake"],
                "authority": {"rights": [], "observe_only": false, "narrowed_by": null},
                "actions": [], "decisions": [], "requests": [], "costs": [],
                "notes": "Cycle 2 resumed conv-live and emitted conv-expired.",
                "next_wake": {"reason": "heartbeat", "not_before": now + Duration::hours(4)},
                "continuity": "resumed", "outcome": "idle", "probe": false,
                "conversation_ref": "conv-expired"
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    assert_eq!(cp2["checkpoint"]["continuity"], "resumed");
    fx.runtime.finish_resident_cycle(&run2).unwrap();
    fx.stop_run(&run2, JobRunState::Success);

    // Cycle 3: expired reference ("conv-expired") -> adapter falls back to brief alone and records continuity: rebuilt.
    let run3 = fx.cycle_run(&mandate, "resident-continuity-3");
    let brief3 = fx
        .runtime
        .begin_resident_cycle(&run3, now + Duration::minutes(260))
        .unwrap();
    assert_eq!(brief3["conversation_ref"], "conv-expired");
    let audit3 = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_test",
        &run3,
        "codex:test".to_string(),
        None,
    )
    .unwrap();
    let outcome3 = dispatch_v2_activity(V2DispatchInput {
        activity_name: "resident_cycle",
        spec: &activity_asset.spec.spec,
        fs_profile: None,
        input: json!({"brief": brief3, "crew": "sol"}),
        audit: audit3,
        run_id: &run3,
        host: Some(&fx.runtime),
    })
    .unwrap();
    assert!(outcome3.success, "{outcome3:?}");
    let cp3 = fx
        .runtime
        .append_resident_checkpoint(
            &mandate,
            json!({
                "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                "cycle": brief3["cycle"], "run_id": run3, "crew": "sol",
                "recovery_of": null, "wake": brief3["wake"],
                "authority": {"rights": [], "observe_only": false, "narrowed_by": null},
                "actions": [], "decisions": [], "requests": [], "costs": [],
                "notes": "Cycle 3 fell back from expired ref.",
                "next_wake": {"reason": "heartbeat", "not_before": now + Duration::hours(6)},
                "continuity": "resumed", "outcome": "idle", "probe": false,
                "conversation_ref": null
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    assert_eq!(
        cp3["checkpoint"]["continuity"], "rebuilt",
        "expired reference must record continuity: rebuilt in checkpoint"
    );
    fx.runtime.finish_resident_cycle(&run3).unwrap();
    fx.stop_run(&run3, JobRunState::Success);

    let logged: Vec<Value> = std::fs::read_to_string(&invocations_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        logged.len(),
        3,
        "expected 1 call for cycle 2 and 2 calls (resume + fallback) for cycle 3: {logged:?}"
    );
    let call1_args: Vec<String> = serde_json::from_value(logged[0]["args"].clone()).unwrap();
    assert!(
        call1_args
            .windows(2)
            .any(|pair| pair == ["resume", "conv-live"]),
        "cycle 2 did not resume conv-live: {call1_args:?}"
    );
    assert_eq!(logged[0]["continuity"], "resumed");
    let call2_args: Vec<String> = serde_json::from_value(logged[1]["args"].clone()).unwrap();
    assert!(
        call2_args
            .windows(2)
            .any(|pair| pair == ["resume", "conv-expired"]),
        "cycle 3 first attempt did not try conv-expired: {call2_args:?}"
    );
    let call3_args: Vec<String> = serde_json::from_value(logged[2]["args"].clone()).unwrap();
    assert!(
        !call3_args
            .iter()
            .any(|arg| arg == "resume" || arg == "conv-expired"),
        "cycle 3 fallback must not pass resume args: {call3_args:?}"
    );
    assert_eq!(logged[2]["continuity"], "rebuilt");
    assert_eq!(logged[2]["conversation_ref"], Value::Null);
}

#[test]
fn instructed_checkpoint_preserves_conversation_ref_across_two_resumed_cycles() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::instructed_checkpoint_preserves_conversation_ref_across_two_resumed_cycles",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 5");
    let mandate = fx.mandate(&watched);
    let now = Utc::now();

    // Setup only: establish the live provider conversation a later cycle
    // can resume. The cycles under test below write no fresh reference.
    let run1 = fx.cycle_run(&mandate, "resident-instructed-seed");
    let brief1 = fx.runtime.begin_resident_cycle(&run1, now).unwrap();
    assert_eq!(brief1["continuity"], "rebuilt");
    assert_eq!(brief1["conversation_ref"], Value::Null);
    fx.runtime
        .append_resident_checkpoint(
            &mandate,
            json!({
                "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                "cycle": brief1["cycle"], "run_id": run1, "crew": "sol",
                "recovery_of": null, "wake": brief1["wake"],
                "authority": {"rights": [], "observe_only": false, "narrowed_by": null},
                "actions": [], "decisions": [], "requests": [], "costs": [],
                "notes": "Seeded the live provider conversation.",
                "next_wake": {"reason": "heartbeat", "not_before": now + Duration::hours(2)},
                "continuity": "rebuilt", "outcome": "idle", "probe": false,
                "conversation_ref": "conv-live"
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    fx.runtime.finish_resident_cycle(&run1).unwrap();
    fx.stop_run(&run1, JobRunState::Success);

    // Two consecutive cycles each write only the instructed checkpoint
    // shape (echoing brief.conversation_ref). Before the template carried
    // conversation_ref, the first of these dropped it and the next brief
    // fell back to rebuilt.
    for (index, key) in ["resident-instructed-1", "resident-instructed-2"]
        .into_iter()
        .enumerate()
    {
        let run = fx.cycle_run(&mandate, key);
        let brief = fx
            .runtime
            .begin_resident_cycle(&run, now + Duration::minutes(130 * (index as i64 + 1)))
            .unwrap();
        assert_eq!(brief["continuity"], "resumed", "cycle {index}: {brief}");
        assert_eq!(brief["conversation_ref"], "conv-live", "cycle {index}");
        let written = fx.checkpoint_as_instructed(&brief, &run);
        assert_eq!(written["checkpoint"]["continuity"], "resumed");
        assert_eq!(written["checkpoint"]["conversation_ref"], "conv-live");
        fx.runtime.finish_resident_cycle(&run).unwrap();
        fx.stop_run(&run, JobRunState::Success);
    }

    // The second instructed checkpoint still leaves a resumable reference.
    let run = fx.cycle_run(&mandate, "resident-instructed-3");
    let brief = fx
        .runtime
        .begin_resident_cycle(&run, now + Duration::minutes(390))
        .unwrap();
    assert_eq!(brief["continuity"], "resumed", "{brief}");
    assert_eq!(brief["conversation_ref"], "conv-live");
    fx.checkpoint_as_instructed(&brief, &run);
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.stop_run(&run, JobRunState::Success);
}

/// Seeds a cycle whose checkpoint carries `conversation_ref`, then dispatches
/// the next resident cycle through a fake `codex` running `provider_body`
/// (Python; `args` is the argv). Returns the dispatch result, the argv of every
/// provider call, and the brief's continuity.
#[cfg(unix)]
fn dispatch_resumed_cycle_with_fake_codex(
    conversation_ref: &str,
    provider_body: &str,
) -> (Result<bool, String>, Vec<Vec<String>>, Value) {
    use orbit_engine::{V2AuditWriter, V2DispatchInput, dispatch_v2_activity, load_activity_asset};
    use orbit_types::resource::ExecutorResource;
    use orbit_types::workflow::ExecutorDef;
    use std::os::unix::fs::PermissionsExt;

    let fx = fixture();
    let bin_dir = tempfile::tempdir().unwrap();
    let program = bin_dir.path().join("codex");
    let invocations_log = bin_dir.path().join("invocations.jsonl");
    let script = format!(
        "#!/bin/sh\ncat >/dev/null\npython3 - '{log}' \"$@\" <<'PY'\nimport json, pathlib, sys\nargs = sys.argv[2:]\nwith pathlib.Path(sys.argv[1]).open('a') as f:\n    f.write(json.dumps(args) + '\\n')\n{body}\nPY\n",
        log = invocations_log.display(),
        body = provider_body
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let resource: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/codex.yaml")).unwrap();
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    def.sandbox = None;
    fx.runtime.upsert_executor_def(&def).unwrap();

    let watched = fx.task("Ship scribe v2 slice 5");
    let mandate = fx.mandate(&watched);
    let now = Utc::now();
    let run1 = fx.cycle_run(&mandate, "resident-resume-seed");
    let brief1 = fx.runtime.begin_resident_cycle(&run1, now).unwrap();
    fx.runtime
        .append_resident_checkpoint(
            &mandate,
            json!({
                "kind": "resident.checkpoint", "schema": 1, "mandate": mandate,
                "cycle": brief1["cycle"], "run_id": run1, "crew": "sol",
                "recovery_of": null, "wake": brief1["wake"],
                "authority": {"rights": [], "observe_only": false, "narrowed_by": null},
                "actions": [], "decisions": [], "requests": [], "costs": [],
                "notes": "seed",
                "next_wake": {"reason": "heartbeat", "not_before": now + Duration::hours(2)},
                "continuity": "rebuilt", "outcome": "idle", "probe": false,
                "conversation_ref": conversation_ref
            }),
            Some("codex".into()),
            None,
        )
        .unwrap();
    fx.runtime.finish_resident_cycle(&run1).unwrap();
    fx.stop_run(&run1, JobRunState::Success);

    let run2 = fx.cycle_run(&mandate, "resident-resume-under-test");
    let brief2 = fx
        .runtime
        .begin_resident_cycle(&run2, now + Duration::minutes(130))
        .unwrap();
    assert_eq!(brief2["conversation_ref"], conversation_ref);
    let mut activity_asset =
        load_activity_asset(include_str!("../../assets/activities/resident_cycle.yaml")).unwrap();
    if let orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(ref mut loop_spec) =
        activity_asset.spec.spec
    {
        loop_spec.provider = orbit_types::workflow::activity_job::Provider::Codex;
        loop_spec.crew_name = Some("sol".into());
    }
    let audit_dir = tempfile::tempdir().unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_test",
        &run2,
        "codex:test".to_string(),
        None,
    )
    .unwrap();
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "resident_cycle",
        spec: &activity_asset.spec.spec,
        fs_profile: None,
        input: json!({"brief": brief2, "crew": "sol"}),
        audit,
        run_id: &run2,
        host: Some(&fx.runtime),
    })
    .map(|outcome| outcome.success)
    .map_err(|error| error.to_string());
    let calls = std::fs::read_to_string(&invocations_log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (outcome, calls, brief2["continuity"].clone())
}

#[cfg(unix)]
#[test]
fn resumed_codex_cycle_with_a_transient_error_frame_is_not_rerun_or_rebuilt() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::resumed_codex_cycle_with_a_transient_error_frame_is_not_rerun_or_rebuilt",
    ) {
        return;
    }
    let (outcome, calls, _) = dispatch_resumed_cycle_with_fake_codex(
        "conv-live",
        r#"
envelope = {"schemaVersion": 1, "status": "success", "result": {"outcome": "idle", "summary": "ok"}, "error": None}
print(json.dumps({"type": "thread.started", "thread_id": "conv-live"}))
print(json.dumps({"type": "error", "message": "Reconnecting... 1/5"}))
print(json.dumps({"type": "item.completed", "item": {"type": "agent_message", "text": json.dumps(envelope)}}))
print(json.dumps({"type": "turn.completed", "usage": {}}))
"#,
    );
    assert_eq!(outcome, Ok(true));
    assert_eq!(calls.len(), 1, "the successful cycle was re-run: {calls:?}");
    assert!(
        calls[0]
            .windows(2)
            .any(|pair| pair == ["resume", "conv-live"]),
        "{calls:?}"
    );
}

#[cfg(unix)]
#[test]
fn resumed_codex_cycle_failing_for_a_non_session_reason_is_not_rerun() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::resumed_codex_cycle_failing_for_a_non_session_reason_is_not_rerun",
    ) {
        return;
    }
    let (outcome, calls, _) = dispatch_resumed_cycle_with_fake_codex(
        "conv-live",
        r#"
print(json.dumps({"type": "error", "message": "unexpected status 401 Unauthorized: token has expired"}))
print(json.dumps({"type": "turn.failed", "error": {"message": "unexpected status 401 Unauthorized"}}))
sys.stderr.write("authentication failed\n")
sys.exit(1)
"#,
    );
    assert_ne!(outcome, Ok(true), "the failure must surface");
    assert_eq!(calls.len(), 1, "the brief was silently re-run: {calls:?}");
}

/// The cycle agent runs the mandate's crew; the run's Resolved Run Crew and
/// recorded model must name that crew, not the assignment task's own field.
#[test]
fn a_cycle_run_records_the_mandate_crew_that_runs() {
    if !super::dispatch_admission::isolated(
        "resident_cycle::a_cycle_run_records_the_mandate_crew_that_runs",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Ship scribe v2 slice 5");
    let mandate = fx.mandate(&watched);
    let run = fx.cycle_run(&mandate, "resident-cycle-crew");
    let before = fx.jobs.get_job_run(&run).unwrap().unwrap();
    assert_eq!(before.resolved_crew, None);
    let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    assert_eq!(brief["crew"], "sol", "{brief}");
    let recorded = fx.jobs.get_job_run(&run).unwrap().unwrap();
    assert_eq!(recorded.resolved_crew.as_deref(), Some("sol"));
    assert_eq!(recorded.crew_model.as_deref(), Some("test"));
}
