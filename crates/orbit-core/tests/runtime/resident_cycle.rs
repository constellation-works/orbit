//! The resident cycle: the `resident_due` trigger fires a mandate only when
//! something it watches changed (or its heartbeat elapsed) and its cadence
//! floor allows, never while a cycle is live; core scopes a cycle's tool
//! calls to its grant; an open maintenance notice makes the cycle
//! observe-only and its closing wakes the mandate; a killed cycle leaves a
//! synthesized `incomplete` checkpoint and its recovery never repeats an
//! action the dead cycle completed.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::automation::evaluate_routine;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_store::contracts::{JobRunStoreBackend, KeyedJobRunAdmission};
use orbit_types::task::TaskArtifact;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{JobRunState, ResidentComment, RoutineDefinition};
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
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
            "watch": {"tasks": [watched]},
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
                    "continuity": "rebuilt", "outcome": "idle", "probe": false
                }),
                Some("codex".into()),
                None,
            )
            .unwrap();
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

#[test]
fn an_open_maintenance_notice_makes_the_cycle_observe_only_and_its_close_wakes_it() {
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
