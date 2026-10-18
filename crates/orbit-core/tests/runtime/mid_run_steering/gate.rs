//! Mid-run steering's promotion gate (design §3, spec invariant 4) in both
//! shipped delivery pipelines.
//!
//! Each test runs the head of `task_local_pipeline` and `task_pr_pipeline` —
//! every step up to and including `commit`, with the asset's own `when:`
//! guards, loop and `steering_gate` — through the job executor. The
//! deterministic steps are scripted. The implement step builds its prompt
//! through the runtime exactly as agent dispatch does, so the gate's
//! reading, its rounds, delivery and the `steering_unconsumed` hold are all
//! the runtime's own records.

use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_core::TaskStatus;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::activity_job::load_job_asset;
use orbit_engine::{
    DispatchError, JobOutcome, RuntimeHost, SteeringGateReading, SteeringRoundAdmission,
    TaskAutomationUpdate, V2AuditWriter, execute_job_with_resume,
};
use orbit_tools::{OwnerCoordinator, ToolContext};
use orbit_types::task::ExecutionLocation;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{CommentLogMark, JobRunState, PipelineState};
use serde_json::{Value, json};

use super::{ENABLED, Fixture};

const PIPELINES: [&str; 2] = ["task_local_pipeline", "task_pr_pipeline"];

/// The shipped `pipeline`'s steps up to and including `commit`, each target
/// replaced by a scripted `probe` labelled with its step id.
fn pipeline_head(pipeline: &str) -> JobV2 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/jobs/{pipeline}.yaml"));
    let asset: Value = serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let steps = asset["spec"]["steps"].as_array().unwrap();
    let commit = steps
        .iter()
        .position(|step| step["id"] == "commit")
        .unwrap();
    let head = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": pipeline },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "default_input": asset["spec"]["default_input"],
            "steering_gate": asset["spec"]["steering_gate"],
            "steps": steps[..=commit].iter().map(probe).collect::<Vec<_>>(),
        },
    });
    let job = load_job_asset(&head.to_string()).unwrap().spec;
    if PIPELINES.contains(&pipeline) {
        assert!(job.steering_gate.is_some(), "{pipeline} declares the gate");
    }
    job
}

fn probe(step: &Value) -> Value {
    let mut step = step.clone();
    let object = step.as_object_mut().unwrap();
    object.remove("recovery_activity");
    object.remove("retry");
    if object.remove("target").is_some() {
        let mut input = object.remove("default_input").unwrap_or_else(|| json!({}));
        input["label"] = object["id"].clone();
        object.insert("default_input".into(), input);
        object.insert(
            "spec".into(),
            json!({ "type": "deterministic", "action": "probe", "config": {} }),
        );
    }
    if let Some(nested) = object
        .get_mut("loop")
        .and_then(|block| block.get_mut("steps"))
        .and_then(Value::as_array_mut)
    {
        for step in nested {
            *step = probe(step);
        }
    }
    step
}

/// What the scripted implement step does after it built its prompt, given
/// how many implement prompts the run built before it.
type OnImplement = Box<dyn Fn(&Fixture, usize) + Send + Sync>;

/// Scripts the deterministic steps; the gate, its rounds, delivery and task
/// writes are the fixture runtime's.
struct Pipeline<'a> {
    fx: &'a Fixture,
    run: String,
    /// `candidate_resume`'s verdict: false carries a candidate past
    /// `implement_bundle`.
    implement: bool,
    on_implement: OnImplement,
    /// The gate reads a task that does not exist, so every read fails.
    unreadable: bool,
    /// The gate has a claimed worker binding and no authoritative owner read.
    claimed: bool,
    /// The implement prompt of a steering round fails to read the task.
    round_read_fails: bool,
    /// An operator comment lands right after the gate's next read.
    comment_after_gate_read: Mutex<bool>,
    calls: Mutex<Vec<(String, Value)>>,
    prompts: Mutex<Vec<Value>>,
}

impl<'a> Pipeline<'a> {
    fn new(fx: &'a Fixture, run: &str) -> Self {
        Self {
            fx,
            run: run.to_string(),
            implement: true,
            on_implement: Box::new(|_, _| {}),
            unreadable: false,
            claimed: false,
            round_read_fails: false,
            comment_after_gate_read: Mutex::new(false),
            calls: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
        }
    }

    fn labels(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(label, _)| label.clone())
            .collect()
    }

    fn implement_inputs(&self) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(label, _)| label == "implement_one")
            .map(|(_, input)| input.clone())
            .collect()
    }

    /// Run `job` to its end; `Err` carries an executor error's text.
    fn try_execute(
        &self,
        job: &JobV2,
        resume: Option<&PipelineState>,
    ) -> Result<JobOutcome, String> {
        let audit = V2AuditWriter::with_disk_sinks(
            &self.fx.workspace.join("audit"),
            std::sync::Arc::new(orbit_store::Store::open_in_memory().unwrap()),
            "ws_fixture",
            &self.run,
            "fixture",
            None,
        )
        .unwrap();
        execute_job_with_resume(
            job,
            json!({ "task_ids": [self.fx.task], "base_branch": "main", "base_sync": "remote" }),
            &self.run,
            audit,
            self,
            resume,
        )
        .map_err(|error| error.to_string())
    }

    fn execute(&self, job: &JobV2, resume: Option<&PipelineState>) -> JobOutcome {
        self.try_execute(job, resume).unwrap()
    }
}

struct UnreachableOwner;

impl OwnerCoordinator for UnreachableOwner {
    fn call(
        &self,
        _name: &str,
        _input: Value,
        _session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::Execution(
            "steering fixture owner is unreachable".into(),
        ))
    }
}

impl orbit_engine::test_support::NullRuntimeHost for Pipeline<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _context: ToolContext,
    ) -> Result<Value, DispatchError> {
        assert_eq!(action, "probe");
        let label = input["label"].as_str().unwrap().to_string();
        self.calls
            .lock()
            .unwrap()
            .push((label.clone(), input.clone()));
        Ok(match label.as_str() {
            "worktree" => json!({
                "job_run_id": self.run,
                "workspace_path": self.fx.workspace,
                "base_sha": "base",
                "base_ref": "main",
                "prior_job_run_id": null,
                "prior_foreign_run": null,
            }),
            "resume_candidate" => json!({ "implement": self.implement, "repair": null }),
            "implement_one" => {
                // Agent dispatch builds the envelope's task context this way.
                let task = if self.round_read_fails && input.get("steering_round").is_some() {
                    "ORB-missing"
                } else {
                    input["task_id"].as_str().unwrap()
                };
                let prompt = RuntimeHost::task_context_for_agent_input(
                    &self.fx.runtime,
                    &self.run,
                    "implement_one",
                    &json!({ "task_id": task }),
                )?
                .unwrap();
                let built = {
                    let mut prompts = self.prompts.lock().unwrap();
                    prompts.push(prompt);
                    prompts.len() - 1
                };
                (self.on_implement)(self.fx, built);
                json!({})
            }
            _ => json!({ "probed": label }),
        })
    }

    fn steering_gate(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<SteeringGateReading, OrbitError> {
        let task = if self.unreadable {
            "ORB-missing"
        } else {
            task_id
        };
        let runtime = if self.claimed {
            self.fx.runtime.clone().with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: "hm_owner".into(),
                    owner_workspace_id: "ws_owner".into(),
                    owner_destination: "owner".into(),
                    task_id: task_id.into(),
                    claim_id: "claim-steering".into(),
                    execution: ExecutionLocation {
                        machine_id: "hm_follower".into(),
                        machine_name: None,
                    },
                    bound_run_id: run_id.into(),
                },
                Arc::new(UnreachableOwner),
            )?
        } else {
            self.fx.runtime.clone()
        };
        let reading = RuntimeHost::steering_gate(&runtime, run_id, task);
        if std::mem::take(&mut *self.comment_after_gate_read.lock().unwrap()) {
            self.fx
                .operator_comment("STEER-LATE: written while the gate's verdict stands.");
        }
        reading
    }

    fn admit_steering_round(
        &self,
        run_id: &str,
        task_id: &str,
        outstanding: &[CommentLogMark],
    ) -> Result<SteeringRoundAdmission, OrbitError> {
        RuntimeHost::admit_steering_round(&self.fx.runtime, run_id, task_id, outstanding)
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        RuntimeHost::apply_task_automation_update(&self.fx.runtime, task_id, update)
    }
}

/// The task, moved to `in-progress` as admission leaves it.
fn admitted(config: &str) -> Fixture {
    let fx = Fixture::new(config);
    for status in [TaskStatus::Backlog, TaskStatus::InProgress] {
        fx.runtime
            .update_task_as_human(
                &fx.task,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                "human:operator".into(),
            )
            .unwrap();
    }
    fx
}

fn head_labels(pipeline: &str) -> Vec<&'static str> {
    let mut labels = vec!["worktree"];
    if pipeline == "task_pr_pipeline" {
        labels.push("review_preflight");
    }
    labels.extend(["resume_candidate", "implement_one"]);
    labels
}

fn with(mut labels: Vec<&'static str>, tail: &[&'static str]) -> Vec<String> {
    labels.extend(tail);
    labels.into_iter().map(str::to_string).collect()
}

fn rounds(fx: &Fixture, run: &str) -> usize {
    fx.state(run)
        .steering_rounds
        .get(&fx.task)
        .map_or(0, Vec::len)
}

fn history_events(fx: &Fixture, event: &str) -> Vec<String> {
    fx.runtime
        .get_task_history(&fx.task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == event)
        .map(|entry| entry.note.unwrap_or_default())
        .collect()
}

fn steering_off_and_nothing_outstanding_leave_both_pipelines_as_they_are() {
    for pipeline in PIPELINES {
        let job = pipeline_head(pipeline);
        let mut ungated = job.clone();
        ungated.steering_gate = None;
        let expected = with(head_labels(pipeline), &["commit"]);

        // Off, an operator comment written mid-run neither stops the run nor
        // leaves a trace; the run is the one the job runs without a gate.
        let mut fx = admitted("");
        let run = fx.run(None);
        let mut outcomes = Vec::new();
        for job in [&job, &ungated] {
            let mut host = Pipeline::new(&fx, &run);
            host.on_implement = Box::new(|fx, _| {
                fx.operator_comment("STEER: written while steering is off.");
            });
            let outcome = host.execute(job, None);
            assert!(outcome.success, "{pipeline}: {outcome:?}");
            assert_eq!(host.labels(), expected, "{pipeline}");
            assert!(fx.state(&run).steering_rounds.is_empty());
            outcomes.push(outcome.pipeline);
        }
        assert_eq!(
            outcomes[0], outcomes[1],
            "{pipeline}: off, the gate is byte-inert"
        );
        assert!(fx.deliveries().is_empty());

        // On, an agent's comment after the snapshot is not steering.
        fx.reconfigure(ENABLED);
        let run = fx.run(None);
        let mut host = Pipeline::new(&fx, &run);
        host.on_implement = Box::new(|fx, _| {
            fx.comment("AGENT: implementation finished.");
        });
        let outcome = host.execute(&job, None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert_eq!(host.labels(), expected, "{pipeline}");
        assert_eq!(rounds(&fx, &run), 0, "{pipeline}: no round was spent");
    }
}

fn a_refusal_spends_one_round_whose_prompt_delivers_and_then_commits() {
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let mut host = Pipeline::new(&fx, &run);
        host.on_implement = Box::new(|fx, built| {
            if built == 0 {
                fx.operator_comment("STEER: rename the flag before this lands.");
            }
        });
        let outcome = host.execute(&pipeline_head(pipeline), None);

        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert_eq!(
            host.labels(),
            with(head_labels(pipeline), &["implement_one", "commit"]),
            "{pipeline}: commit is reached only after the round"
        );
        let inputs = host.implement_inputs();
        assert!(inputs[0].get("steering_round").is_none());
        assert_eq!(inputs[1]["steering_round"]["round"], 1, "{pipeline}");
        assert_eq!(inputs[1]["steering_round"]["max_rounds"], 2, "{pipeline}");
        let prompts = host.prompts.lock().unwrap();
        let block = prompts[1]["steering"]["comments"].as_str().unwrap();
        assert!(block.contains("STEER: rename the flag"), "{pipeline}");
        assert_eq!(fx.deliveries().len(), 1, "{pipeline}: delivered once");
        assert_eq!(rounds(&fx, &run), 1, "{pipeline}");
        assert_eq!(
            RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task).unwrap(),
            SteeringGateReading::Clear
        );
    }

    // A carried candidate skips `implement_bundle`, so the run built no
    // prompt and holds no watermark: an operator comment after it started is
    // undelivered, and the round runs the implementer despite the guard.
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        fx.operator_comment("STEER: the carried candidate needs a test.");
        let mut host = Pipeline::new(&fx, &run);
        host.implement = false;
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        let mut expected = head_labels(pipeline);
        expected.pop();
        assert_eq!(
            host.labels(),
            with(expected, &["implement_one", "commit"]),
            "{pipeline}"
        );
        assert_eq!(rounds(&fx, &run), 1, "{pipeline}");
    }

    // A resume with `implement_bundle` checkpointed builds no prompt of its
    // own. The run it resumes carried the candidate's prompt, so a comment
    // that run never delivered is outstanding even though it predates the
    // resume, and the round dispatches the implementer again.
    let fx = admitted(ENABLED);
    let source = fx.run(None);
    fx.prompt(&source, "implement_one");
    fx.operator_comment("STEER: arrived while the run was down.");
    let run = fx.run(Some(&source));
    let job = pipeline_head("task_local_pipeline");
    let commit = job
        .steps
        .iter()
        .position(|step| step.id == "commit")
        .unwrap();
    let mut resume = fx.state(&run);
    for (index, step) in job.steps[..commit].iter().enumerate() {
        let output = match step.id.as_str() {
            // No recorded base: nothing about the worktree's handoff needs
            // reconciling before the resumed steps run.
            "worktree" => json!({ "job_run_id": run, "workspace_path": fx.workspace,
                "base_sha": null, "base_ref": "main" }),
            "resume_candidate" => json!({ "implement": true, "repair": null }),
            _ => json!({}),
        };
        resume.record_step(
            index as u32,
            JobRunState::Success,
            Some(output.clone()),
            None,
        );
        resume.record_pipeline_output(&step.id, output);
    }
    let host = Pipeline::new(&fx, &run);
    let outcome = host.execute(&job, Some(&resume));
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(host.labels(), ["implement_one", "commit"]);
    assert_eq!(rounds(&fx, &run), 1);
    assert!(
        host.prompts.lock().unwrap()[0]["steering"]["comments"]
            .as_str()
            .is_some_and(|block| block.contains("STEER: arrived while the run was down"))
    );
    assert_eq!(fx.deliveries().len(), 1);
}

// A carried candidate builds its first prompt in the admitted steering round.
// Ordinary task context keeps only the newest twenty comments, so it cannot
// substitute for delivering the comments the gate found outstanding.
fn a_first_prompt_steering_round_delivers_comments_omitted_from_task_context() {
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        fx.operator_comment("STEER-OLDEST: cover the public failure path.");
        for _ in 0..20 {
            fx.comment("AGENT: progress update.");
        }
        let mut host = Pipeline::new(&fx, &run);
        host.implement = false;
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        let prompts = host.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "{pipeline}");
        assert_eq!(prompts[0]["comments_omitted_count"], 1);
        assert!(
            prompts[0]["steering"]["comments"]
                .as_str()
                .is_some_and(|block| block.contains("STEER-OLDEST")),
            "{pipeline}: the gate's oldest outstanding comment must reach the first round's prompt"
        );
        assert_eq!(fx.deliveries().len(), 1, "{pipeline}");
        assert_eq!(rounds(&fx, &run), 1, "{pipeline}");
    }
}

fn a_first_prompt_steering_round_keeps_truncated_guidance_outstanding() {
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        fx.operator_comment(&format!("STEER-HUGE {}", "x".repeat(40 * 1024)));
        let mut host = Pipeline::new(&fx, &run);
        host.implement = false;
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(
            !outcome.success,
            "{pipeline}: a truncated first prompt cannot clear the gate"
        );
        assert!(
            !host.labels().iter().any(|label| label == "commit"),
            "{pipeline}"
        );
        assert_eq!(rounds(&fx, &run), 1, "{pipeline}");
        assert_eq!(
            outcome.pipeline["commit"]["decision"],
            "steering_unconsumed"
        );
        assert_eq!(fx.deliveries()[0]["delivered"], json!([]));
    }
}

fn claimed_pipelines_fail_closed_when_the_gate_cannot_certify_the_task() {
    for pipeline in ["task_claimed_local_pipeline", "task_claimed_pr_pipeline"] {
        for config in ["", ENABLED] {
            let fx = admitted(config);
            let run = fx.run(None);
            let mut host = Pipeline::new(&fx, &run);
            host.claimed = true;
            host.on_implement = Box::new(|fx, _| {
                fx.operator_comment("STEER: the replica cannot certify delivery.");
            });
            let outcome = host.execute(&pipeline_head(pipeline), None);
            if config.is_empty() {
                assert!(outcome.success, "{pipeline}: steering off remains inert");
                assert!(host.labels().iter().any(|label| label == "commit"));
            } else {
                assert!(
                    !outcome.success,
                    "{pipeline}: an uncertifiable comment log cannot commit"
                );
                assert!(
                    !host.labels().iter().any(|label| label == "commit"),
                    "{pipeline}"
                );
                assert_eq!(outcome.pipeline["commit"]["decision"], "unreadable");
                assert!(
                    outcome
                        .message
                        .unwrap()
                        .contains("replica is never certified")
                );
            }
            assert_eq!(rounds(&fx, &run), 0, "{pipeline}");
        }
    }
}

// A bounded ancestry walk cannot certify a baseline it never reached.
fn an_unresolved_resume_lineage_fails_closed_instead_of_forgetting_steering() {
    let fx = admitted(ENABLED);
    let mut run = fx.run(None);
    fx.prompt(&run, "implement_one");
    fx.operator_comment("STEER-ANCESTOR: this candidate still needs the correction.");
    for _ in 0..33 {
        run = fx.run(Some(&run));
    }
    for pipeline in PIPELINES {
        let mut host = Pipeline::new(&fx, &run);
        host.implement = false;
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(
            !outcome.success,
            "{pipeline}: an unresolved baseline cannot permit commit"
        );
        assert!(
            !host.labels().iter().any(|label| label == "commit"),
            "{pipeline}"
        );
        assert_eq!(outcome.pipeline["commit"]["decision"], "unreadable");
        assert_eq!(rounds(&fx, &run), 0, "{pipeline}");
    }
}

fn exhausted_or_unobservable_rounds_block_the_task_as_steering_unconsumed() {
    for pipeline in PIPELINES {
        // Every implement step meets a new operator comment, so each gate
        // finds steering the last round could not carry.
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let mut host = Pipeline::new(&fx, &run);
        host.on_implement = Box::new(|fx, built| {
            fx.operator_comment(&format!("STEER-{built}: one more change."));
        });
        let outcome = host.execute(&pipeline_head(pipeline), None);

        assert!(!outcome.success, "{pipeline}: {outcome:?}");
        assert!(
            !host.labels().iter().any(|label| label == "commit"),
            "{pipeline}: commit is never reached: {:?}",
            host.labels()
        );
        assert_eq!(host.implement_inputs().len(), 3, "{pipeline}: two rounds");
        assert_eq!(rounds(&fx, &run), 2, "{pipeline}");
        let message = outcome.message.unwrap();
        assert!(message.contains("[steering_unconsumed]"), "{message}");
        let task = fx.runtime.get_task(&fx.task).unwrap();
        assert_eq!(task.status, TaskStatus::Blocked, "{pipeline}");
        let notes = history_events(&fx, "steering_unconsumed");
        assert_eq!(notes.len(), 1, "{pipeline}");
        let last = fx.runtime.get_task_comments(&fx.task).unwrap();
        let outstanding = last
            .iter()
            .rposition(|comment| comment.message.starts_with("STEER-2"))
            .unwrap();
        assert!(
            notes[0].contains(&format!("comment {outstanding} at")),
            "{pipeline}: the hold names the outstanding comment: {}",
            notes[0]
        );
        assert_eq!(
            outcome.pipeline["commit"]["decision"], "steering_unconsumed",
            "{pipeline}"
        );
    }

    // A comment too long for any block is never carried in full: the round
    // admitted for it delivers none of it, so no second round is spent on it.
    let fx = admitted(ENABLED);
    let run = fx.run(None);
    let mut host = Pipeline::new(&fx, &run);
    host.on_implement = Box::new(|fx, built| {
        if built == 0 {
            fx.operator_comment(&format!("STEER-HUGE {}", "x".repeat(40 * 1024)));
        }
    });
    let outcome = host.execute(&pipeline_head("task_pr_pipeline"), None);
    assert!(!outcome.success);
    assert_eq!(host.implement_inputs().len(), 2, "one round, not two");
    assert_eq!(rounds(&fx, &run), 1);
    assert!(outcome.message.unwrap().contains("delivered none of them"));
    assert_eq!(
        fx.runtime.get_task(&fx.task).unwrap().status,
        TaskStatus::Blocked
    );
}

fn a_round_that_cannot_read_the_task_is_spent_and_keeps_its_own_failure() {
    let fx = admitted(ENABLED);
    let run = fx.run(None);
    let mut host = Pipeline::new(&fx, &run);
    host.round_read_fails = true;
    host.on_implement = Box::new(|fx, built| {
        if built == 0 {
            fx.operator_comment("STEER: rename the flag.");
        }
    });
    let message = match host.try_execute(&pipeline_head("task_local_pipeline"), None) {
        Ok(outcome) => outcome.message.unwrap_or_default(),
        Err(error) => error,
    };
    assert!(
        message.contains("for agent envelope"),
        "the round's own failure stands: {message}"
    );
    assert_eq!(rounds(&fx, &run), 1, "the admitted round is spent");
    assert!(fx.deliveries().is_empty(), "and delivered nothing");
    assert!(!host.labels().iter().any(|label| label == "commit"));
    assert_ne!(
        fx.runtime.get_task(&fx.task).unwrap().status,
        TaskStatus::Blocked,
        "a failed round is not a steering hold"
    );
}

fn an_unreadable_gate_fails_closed_and_a_later_comment_waits_for_the_next_gate() {
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let mut host = Pipeline::new(&fx, &run);
        host.unreadable = true;
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(!outcome.success, "{pipeline}");
        assert!(
            outcome
                .message
                .unwrap()
                .contains("[steering_gate_unreadable]")
        );
        assert!(!host.labels().iter().any(|label| label == "commit"));
        assert_eq!(
            rounds(&fx, &run),
            0,
            "{pipeline}: no round on a failed read"
        );
        assert_eq!(
            fx.runtime.get_task(&fx.task).unwrap().status,
            TaskStatus::InProgress
        );

        // A comment written after the gate read its verdict is not covered
        // by it: the run commits, and the comment stays undelivered.
        let run = fx.run(None);
        let host = Pipeline::new(&fx, &run);
        *host.comment_after_gate_read.lock().unwrap() = true;
        let outcome = host.execute(&pipeline_head(pipeline), None);
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert_eq!(host.labels(), with(head_labels(pipeline), &["commit"]));
        assert!(fx.deliveries().is_empty(), "{pipeline}");
        let late = fx.runtime.get_task_comments(&fx.task).unwrap().len() - 1;
        let SteeringGateReading::Outstanding(outstanding) =
            RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task).unwrap()
        else {
            panic!("{pipeline}: the late comment is the next gate's steering");
        };
        assert_eq!(
            outstanding
                .iter()
                .map(|mark| mark.position)
                .collect::<Vec<_>>(),
            [late as u64]
        );
        assert!(!fx.watermark(&run).covers(late as u64));
    }
}

fn a_terminal_task_stops_the_run_and_reports_its_outstanding_steering() {
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let mut host = Pipeline::new(&fx, &run);
        host.on_implement = Box::new(|fx, _| {
            fx.operator_comment("STEER: never mind, this is done elsewhere.");
            RuntimeHost::apply_task_automation_update(
                &fx.runtime,
                &fx.task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Archived),
                    ..TaskAutomationUpdate::default()
                },
            )
            .unwrap();
        });
        let outcome = host.execute(&pipeline_head(pipeline), None);

        assert!(!outcome.success, "{pipeline}");
        assert_eq!(
            host.labels(),
            with(head_labels(pipeline), &[]),
            "{pipeline}: no round and no commit"
        );
        let gate = &outcome.pipeline["commit"];
        assert_eq!(gate["decision"], "task_terminal", "{pipeline}");
        assert_eq!(gate["task_status"], "archived");
        assert_eq!(
            gate["outstanding"].as_array().map(Vec::len),
            Some(1),
            "{pipeline}: the step outcome names the outstanding steering"
        );
        assert_eq!(
            fx.runtime.get_task(&fx.task).unwrap().status,
            TaskStatus::Archived
        );
        assert!(history_events(&fx, "steering_unconsumed").is_empty());
        assert_eq!(rounds(&fx, &run), 0);
    }
}

crate::scenario_table!(
    mid_run_steering_gate_scenarios,
    steering_off_and_nothing_outstanding_leave_both_pipelines_as_they_are,
    a_refusal_spends_one_round_whose_prompt_delivers_and_then_commits,
    a_first_prompt_steering_round_delivers_comments_omitted_from_task_context,
    a_first_prompt_steering_round_keeps_truncated_guidance_outstanding,
    claimed_pipelines_fail_closed_when_the_gate_cannot_certify_the_task,
    an_unresolved_resume_lineage_fails_closed_instead_of_forgetting_steering,
    exhausted_or_unobservable_rounds_block_the_task_as_steering_unconsumed,
    a_round_that_cannot_read_the_task_is_spent_and_keeps_its_own_failure,
    an_unreadable_gate_fails_closed_and_a_later_comment_waits_for_the_next_gate,
    a_terminal_task_stops_the_run_and_reports_its_outstanding_steering
);
