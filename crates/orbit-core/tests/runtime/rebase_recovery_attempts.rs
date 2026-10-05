//! [F2026-10-041] The runtime's side of repeated conflict recovery on one
//! step, through the `RuntimeHost` methods the engine calls: each admitted
//! recovery reserves a host-assigned attempt, certification precedes the
//! run-store copy, and only the newest certified attempt vouches for the step
//! — across a leaf rewriting the run store, a restart, and a persist failure.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_engine::{RebaseRecoveryAttemptScope, RuntimeHost};
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};
use tempfile::TempDir;

const STEP: &str = "sync_base";
const CANDIDATE: &str = "1111111111111111111111111111111111111111";
const PINNED: &str = "2222222222222222222222222222222222222222";
const ADVANCED: &str = "3333333333333333333333333333333333333333";
const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HEAD_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    run: String,
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let input = json!({ "task_ids": ["T-REBASE"] });
    let run = jobs
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(input.clone()), None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    jobs.write_run_state(
        &run.run_id,
        &PipelineState::new(run.run_id.clone(), run.job_id, input),
    )
    .unwrap();
    Fixture {
        _root: root,
        global,
        repo,
        runtime,
        jobs,
        run: run.run_id,
    }
}

impl Fixture {
    fn workspace(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }

    /// Admit a recovery of the stopped rebase from `head_before` onto `pinned`.
    fn admit(&self, head_before: &str, pinned: &str) -> u64 {
        self.runtime
            .begin_rebase_recovery_attempt(
                &self.run,
                STEP,
                &RebaseRecoveryAttemptScope {
                    workspace_path: self.workspace(),
                    head_sha_before: head_before.to_string(),
                    target_base_sha: pinned.to_string(),
                },
            )
            .unwrap()
    }

    /// The host-built completion of `attempt`, as the boundary guard emits it.
    fn completion(&self, attempt: u64, head_before: &str, pinned: &str, head: &str) -> Value {
        json!({
            "run_id": self.run,
            "step_id": STEP,
            "task_ids": ["T-REBASE"],
            "workspace_path": self.workspace(),
            "head": "orbit/T-REBASE",
            "head_sha_before": head_before,
            "original_base_sha": PINNED,
            "base_ref": "main",
            "target_base_sha": pinned,
            "base_sha": pinned,
            "remote_sha_before": Value::Null,
            "head_sha": head,
            "companion_paths": [],
            "rewritten": true,
            "recovery_attempt": attempt,
        })
    }

    fn stored(&self) -> Option<Value> {
        self.jobs
            .read_run_state(&self.run)
            .unwrap()
            .unwrap()
            .rebase_recovery_checkpoints
            .get(STEP)
            .cloned()
    }

    /// Write `checkpoint` into the run store, as a leaf holding the store's
    /// modify grant can.
    fn leaf_writes(&self, checkpoint: Value) {
        let mut state = self.jobs.read_run_state(&self.run).unwrap().unwrap();
        state
            .rebase_recovery_checkpoints
            .insert(STEP.to_string(), checkpoint);
        self.jobs.write_run_state(&self.run, &state).unwrap();
    }

    fn verifies(&self, checkpoint: &Value) -> bool {
        self.runtime
            .verify_rebase_recovery(&self.run, STEP, checkpoint)
            .unwrap()
    }
}

#[test]
fn a_resumed_run_certifies_its_second_recovery_of_one_step() {
    let fixture = fixture();

    // Recovery A lands the candidate on the pin; the advanced base conflicts
    // again, so A keeps the pinned result.
    let attempt = fixture.admit(CANDIDATE, PINNED);
    let recovery_a = fixture.completion(attempt, CANDIDATE, PINNED, HEAD_A);
    fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &recovery_a)
        .unwrap();
    assert_eq!(fixture.stored(), Some(recovery_a.clone()));
    assert!(fixture.verifies(&recovery_a));

    // The same run resumes from preparation, re-pins to the advanced base,
    // and conflicts on the same step: recovery B is a new attempt.
    let attempt = fixture.admit(HEAD_A, ADVANCED);
    assert_eq!(attempt, 2);
    let recovery_b = fixture.completion(attempt, HEAD_A, ADVANCED, HEAD_B);
    fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &recovery_b)
        .expect("a second legitimate recovery of the step certifies");
    assert_eq!(fixture.stored(), Some(recovery_b.clone()));
    assert!(fixture.verifies(&recovery_b));
    assert!(
        !fixture.verifies(&recovery_a),
        "A no longer vouches for the step once B is certified"
    );

    // A leaf rewrites the run store: restoring A, or editing B, buys nothing.
    fixture.leaf_writes(recovery_a.clone());
    assert!(!fixture.verifies(&fixture.stored().unwrap()));
    let mut forged = recovery_b.clone();
    forged["base_sha"] = json!(PINNED);
    fixture.leaf_writes(forged.clone());
    assert!(!fixture.verifies(&forged));
    let mut forged = recovery_b.clone();
    forged["recovery_attempt"] = json!(3);
    assert!(!fixture.verifies(&forged));
    fixture.leaf_writes(recovery_b.clone());

    // Authority outlives the process.
    let restarted =
        OrbitRuntime::from_roots(&fixture.global, &fixture.repo.join(".orbit")).unwrap();
    assert!(
        restarted
            .verify_rebase_recovery(&fixture.run, STEP, &recovery_b)
            .unwrap()
    );
    assert!(
        !restarted
            .verify_rebase_recovery(&fixture.run, STEP, &recovery_a)
            .unwrap()
    );
    // A recovery admitted before the restart cannot certify after a newer one.
    let error =
        RuntimeHost::checkpoint_rebase_recovery(&restarted, &fixture.run, STEP, &recovery_a)
            .expect_err("a superseded attempt never certifies");
    assert!(error.to_string().contains("superseded"), "{error}");
    assert_eq!(fixture.stored(), Some(recovery_b));
}

/// The host certifies, then fails to persist the run-store copy. The step is
/// left with no usable evidence rather than the older attempt's, and the
/// identical completion persists once the store accepts writes again.
#[test]
fn a_certified_attempt_whose_copy_failed_is_reissued_unchanged() {
    let fixture = fixture();
    let attempt = fixture.admit(CANDIDATE, PINNED);
    let recovery_a = fixture.completion(attempt, CANDIDATE, PINNED, HEAD_A);
    fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &recovery_a)
        .unwrap();

    let attempt = fixture.admit(HEAD_A, ADVANCED);
    let recovery_c = fixture.completion(attempt, HEAD_A, ADVANCED, HEAD_C);
    let fault = fail_writes_naming(&fixture, HEAD_C);
    let error = fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &recovery_c)
        .expect_err("the run-store copy fails");
    assert!(
        error
            .to_string()
            .contains("persist rebase recovery checkpoint"),
        "{error}"
    );
    assert_eq!(fixture.stored(), Some(recovery_a.clone()));
    assert!(
        !fixture.verifies(&recovery_a),
        "the older attempt must not stand in for the newer certified one"
    );

    drop(fault);
    fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &recovery_c)
        .expect("the identical completion is re-issued and persisted");
    assert_eq!(fixture.stored(), Some(recovery_c.clone()));
    assert!(fixture.verifies(&recovery_c));
}

/// Fail every run-state write whose state names `marker`, as a crash or a full
/// disk would between certification and the run-store copy. Dropping the
/// returned connection's trigger restores the store.
fn fail_writes_naming(fixture: &Fixture, marker: &str) -> WriteFault {
    let db = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
        &fixture.global,
        fixture.repo.join(".orbit"),
    ))
    .unwrap();
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_recovery_copy \
             BEFORE UPDATE OF pipeline_state_json ON job_runs \
             WHEN NEW.pipeline_state_json LIKE '%{marker}%' \
             BEGIN SELECT RAISE(ABORT, 'injected run-state write failure'); END;"
        ))
        .unwrap();
    WriteFault(connection)
}

struct WriteFault(rusqlite::Connection);

impl Drop for WriteFault {
    fn drop(&mut self) {
        self.0
            .execute_batch("DROP TRIGGER IF EXISTS fail_recovery_copy;")
            .unwrap();
    }
}
