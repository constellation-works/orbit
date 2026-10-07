//! [ORB-14617] A forge outage at the delivery push, run through
//! `execute_job_with_resume` with the real `git_push` action.
//!
//! The remote is a bare repository behind a `git` on `PATH` that refuses the
//! first pushes the way GitHub did during the 2026-10-07 outage
//! (`! [remote rejected] … (Internal Server Error)`) and then hands every
//! command to the real Git. The job is `implement → review → push → open_pull_request`
//! with a step recovery on `push`, a failure activity and a final recovery
//! activity, all scripted, so the test sees whether any of them ran.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^forge_hold::/)'`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use chrono::{TimeDelta, Utc};
use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, JobOutcome, RuntimeHost,
    V2AuditWriter, execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{ActivityV2, JobV2};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

const RUN_ID: &str = "jrun-forge-hold";
const BRANCH: &str = "candidate";
/// More refusals than any budget here asks for (and inside shell arithmetic).
const ALWAYS: usize = 1_000_000;

/// What GitHub printed for every push during the outage.
const INTERNAL_SERVER_ERROR: &str = "remote: Internal Server Error\n\
To https://github.com/constellation-works/orbit.git\n \
! [remote rejected]     candidate -> candidate (Internal Server Error)\n\
error: failed to push some refs to 'https://github.com/constellation-works/orbit.git'";

/// A checkout of a bare remote, with a candidate commit on [`BRANCH`] and a
/// `git` wrapper that refuses the next `refusals` pushes.
struct ForgeFixture {
    root: tempfile::TempDir,
    path: String,
    head: String,
}

impl ForgeFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("forge fixture");
        let remote = root.path().join("remote.git");
        let work = root.path().join("work");
        git(
            root.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        git(
            root.path(),
            &["init", "-q", "-b", BRANCH, work.to_str().unwrap()],
        );
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        fs::write(work.join("candidate.txt"), "the reviewed candidate\n").unwrap();
        git(&work, &["add", "candidate.txt"]);
        git(
            &work,
            &[
                "-c",
                "user.name=Orbit",
                "-c",
                "user.email=orbit@example.invalid",
                "commit",
                "-q",
                "-m",
                "candidate",
            ],
        );
        let head = git(&work, &["rev-parse", "HEAD"]);

        let real_git = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("locate git")
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let state = root.path().display().to_string();
        let wrapper = format!(
            r#"#!/bin/sh
command=
skip=
for argument in "$@"; do
  if [ -n "$skip" ]; then skip=; continue; fi
  if [ "$argument" = -c ]; then skip=1; continue; fi
  command=$argument
  break
done
if [ "$command" = push ]; then
  count=$(($(cat '{state}/pushes') + 1))
  printf '%s\n' "$count" > '{state}/pushes'
  if [ "$count" -le "$(cat '{state}/refusals')" ]; then
    cat '{state}/refusal' >&2
    exit 1
  fi
fi
exec '{real_git}' "$@"
"#
        );
        let wrapper_path = bin.join("git");
        fs::write(&wrapper_path, wrapper).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.path().join("refusal"), INTERNAL_SERVER_ERROR).unwrap();

        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths)
            .unwrap()
            .into_string()
            .expect("PATH is UTF-8");
        let fixture = Self { root, path, head };
        fixture.refuse(0);
        fixture
    }

    /// Refuse the next `refusals` pushes, then accept.
    fn refuse(&self, refusals: usize) {
        fs::write(self.root.path().join("pushes"), "0").unwrap();
        fs::write(self.root.path().join("refusals"), refusals.to_string()).unwrap();
    }

    fn pushes(&self) -> usize {
        fs::read_to_string(self.root.path().join("pushes"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("work")
    }

    /// The commit the remote's branch holds, if any.
    fn remote_head(&self) -> Option<String> {
        let output = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{BRANCH}"),
            ])
            .current_dir(self.root.path().join("remote.git"))
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_string())
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Plays every scripted activity, and checkpoints completed steps the way
/// Core persists them, so a later run can resume from them.
#[derive(Default)]
struct ForgeHost {
    calls: Mutex<Vec<(String, Value)>>,
    checkpoints: Mutex<BTreeMap<u32, (String, Value)>>,
    final_recovery_requests: Mutex<Vec<FinalRecoveryAdmissionRequest>>,
}

impl ForgeHost {
    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(action, _)| action.clone())
            .collect()
    }

    fn inputs(&self, action: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn checkpoint(&self, step_id: &str) -> Option<Value> {
        self.checkpoints
            .lock()
            .unwrap()
            .values()
            .find(|(id, _)| id == step_id)
            .map(|(_, output)| output.clone())
    }

    /// The state Core would persist for this run once it finished with
    /// `outcome`, re-keyed as a resume seeds it.
    fn persisted_state(&self, outcome: &JobOutcome) -> PipelineState {
        let mut state = PipelineState::new(
            RUN_ID.to_string(),
            "forge_hold_fixture".to_string(),
            json!({ "task_ids": ["T-1"] }),
        );
        for (index, (_, output)) in self.checkpoints.lock().unwrap().iter() {
            state.record_step(*index, JobRunState::Success, Some(output.clone()), None);
        }
        state.sync_pipeline(outcome.pipeline.clone());
        state.forge_hold = outcome.forge_hold.clone();
        state
    }
}

impl RuntimeHost for ForgeHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .unwrap()
            .push((action.to_string(), input.clone()));
        Ok(match action {
            "decide" => json!({
                "decision": "escalate",
                "diagnosis": "the forge refused the push",
                "human_action": "restore push acceptance",
            }),
            other => json!({ "action": other }),
        })
    }

    fn checkpoint_step(
        &self,
        _run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
        _compound_outputs: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        self.checkpoints
            .lock()
            .unwrap()
            .insert(step_index, (step_id.to_string(), output.clone()));
        Ok(())
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.final_recovery_requests
            .lock()
            .unwrap()
            .push(request.clone());
        Ok(FinalRecoveryAdmission::Admitted)
    }
}

fn deterministic_activity(name: &str) -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": name },
        "spec": { "type": "deterministic", "description": name, "action": name, "config": {} },
    });
    load_activity_asset(&asset.to_string()).unwrap().spec
}

/// `implement → review → push → open_pull_request`; `push` is the shipped action with
/// a `forge_retry` budget of `max_attempts` short waits.
fn delivery(fixture: &ForgeFixture, max_attempts: u32) -> JobV2 {
    let stub = |id: &str| json!({ "id": id, "spec": { "type": "deterministic", "action": id, "config": {} } });
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "forge_hold_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [
                stub("implement"),
                stub("review"),
                {
                    "id": "push",
                    "spec": { "type": "deterministic", "action": "git_push", "config": {} },
                    "default_input": {
                        "workspace_path": fixture.workspace(),
                        "branch": BRANCH,
                        "forge_retry": {
                            "max_attempts": max_attempts,
                            "initial_backoff_ms": 20,
                            "backoff_cap_ms": 40,
                        },
                    },
                },
                {
                    "id": "open_pull_request",
                    "spec": { "type": "deterministic", "action": "open_pull_request", "config": {} },
                    "default_input": { "head_sha": "{{ steps.push.output.local_sha }}" },
                },
            ],
        },
    });
    let mut job = load_job_asset(&asset.to_string()).unwrap().spec;
    job.steps[2].recovery_activity = Some("step_fix".to_string());
    job.steps[2].resolved_recovery_activity = Some(deterministic_activity("step_fix"));
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));
    job
}

fn execute(job: &JobV2, host: &ForgeHost, resume: Option<&PipelineState>) -> JobOutcome {
    let blobs = tempfile::tempdir().unwrap();
    let audit = Arc::new(V2AuditWriter::new(
        RUN_ID,
        "test-agent",
        Arc::new(InMemorySink::new(blobs.path().join("blobs"))),
    ));
    execute_job_with_resume(
        job,
        json!({ "task_ids": ["T-1"] }),
        RUN_ID,
        audit,
        host,
        resume,
    )
    .expect("the run ends with an outcome, not a dispatch error")
}

#[test]
fn an_outage_inside_the_backoff_budget_costs_waits_and_no_step_failure() {
    let fixture = ForgeFixture::new();
    let _path = orbit_common::test_env::scoped([("PATH", Some(fixture.path.as_str()))]);
    fixture.refuse(3);
    let job = delivery(&fixture, 4);
    let host = ForgeHost::default();

    let outcome = execute(&job, &host, None);

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.forge_hold, None);
    assert_eq!(fixture.pushes(), 4, "three refusals, then the push lands");
    assert_eq!(
        fixture.remote_head().as_deref(),
        Some(fixture.head.as_str())
    );
    assert_eq!(host.actions(), ["implement", "review", "open_pull_request"]);
    let push = host.checkpoint("push").expect("push is checkpointed");
    assert_eq!(push["push_attempts"], 4, "{push}");
    // Three waits, each within half to all of its capped step (20, 40, 40).
    let waited = push["push_waited_ms"]
        .as_u64()
        .expect("the wait is recorded");
    assert!((50..=100).contains(&waited), "waited {waited} ms: {push}");
}

#[test]
fn an_outage_past_the_budget_holds_the_run_and_a_resume_pushes_the_same_head() {
    let fixture = ForgeFixture::new();
    let _path = orbit_common::test_env::scoped([("PATH", Some(fixture.path.as_str()))]);
    fixture.refuse(ALWAYS);
    let job = delivery(&fixture, 3);
    let held = ForgeHost::default();

    let outcome = execute(&job, &held, None);

    assert!(!outcome.success);
    let hold = outcome.forge_hold.clone().expect("a typed forge hold");
    assert_eq!(hold.step_id, "push");
    assert_eq!(hold.head_sha, fixture.head);
    assert_eq!(hold.target_ref, format!("refs/heads/{BRANCH}"));
    assert_eq!(hold.attempts, 3);
    assert!(
        hold.diagnostic.contains("(Internal Server Error)"),
        "{hold:?}"
    );
    assert_eq!(fixture.pushes(), 3);
    assert_eq!(fixture.remote_head(), None);
    assert_eq!(
        held.actions(),
        ["implement", "review"],
        "no step recovery, failure handoff, final recovery or pull request"
    );
    assert!(held.final_recovery_requests.lock().unwrap().is_empty());
    assert_eq!(
        outcome.pipeline["push"]["forge_hold"]["head_sha"],
        fixture.head.as_str(),
        "the run state names the held push"
    );

    // The forge is still down when the clock first resumes: the resumed run
    // holds again and keeps the lineage's first hold time.
    let mut state = held.persisted_state(&outcome);
    state.forge_hold.as_mut().unwrap().held_since = hold.held_at - TimeDelta::minutes(30);
    fixture.refuse(ALWAYS);
    let still_down = ForgeHost::default();
    let again = execute(&job, &still_down, Some(&state));
    let again_hold = again.forge_hold.expect("still held");
    assert_eq!(again_hold.held_since, hold.held_at - TimeDelta::minutes(30));
    assert!(again_hold.held_at >= hold.held_at);
    assert!(
        still_down.actions().is_empty(),
        "{:?}",
        still_down.actions()
    );

    // The forge accepts again: the resume pushes the same head and opens the
    // PR without implementing or reviewing again.
    fixture.refuse(0);
    let state = held.persisted_state(&outcome);
    let resumed = ForgeHost::default();
    let delivered = execute(&job, &resumed, Some(&state));

    assert!(delivered.success, "{:?}", delivered.message);
    assert_eq!(delivered.forge_hold, None);
    assert_eq!(
        fixture.remote_head().as_deref(),
        Some(fixture.head.as_str())
    );
    assert_eq!(resumed.actions(), ["open_pull_request"]);
    assert_eq!(
        resumed.inputs("open_pull_request")[0]["head_sha"],
        fixture.head.as_str()
    );
    assert!(Utc::now() >= hold.held_at);
}
