//! [F2026-10-041] The runtime's side of repeated conflict recovery on one
//! step, through the `RuntimeHost` methods the engine calls: each admitted
//! recovery reserves a host-assigned attempt, certification precedes the
//! run-store copy, and only the newest certified attempt vouches for the step
//! — across a leaf rewriting the run store, a restart, and a persist failure.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::JobRuns;
use orbit_engine::{RebaseRecoveryAttemptScope, RuntimeHost};
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
    jobs: Arc<JobRuns>,
    run: String,
}

fn fixture() -> Fixture {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_core::runtime::JobRuns::shared(
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

fn a_resumed_run_certifies_its_second_recovery_of_one_step_scenario() {
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
fn a_certified_attempt_whose_copy_failed_is_reissued_unchanged_scenario() {
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

/// [ORB-14393] Completion bounds how often it follows a moving base by the
/// distinct bases the host reserved recovery attempts for. The host reports
/// every reservation of a step, oldest first, from its own record, across a
/// restart; and the clean chase a retry certifies as the step's newest attempt
/// supersedes the recovery it carried forward.
fn the_host_reports_each_reserved_attempt_and_certifies_a_chase_as_the_newest_scenario() {
    let fixture = fixture();
    let attempt = fixture.admit(CANDIDATE, PINNED);
    let recovery = fixture.completion(attempt, CANDIDATE, PINNED, HEAD_A);
    fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &recovery)
        .unwrap();

    // The base advanced past the recovery; the retry carries HEAD_A onto it.
    let attempt = fixture.admit(CANDIDATE, ADVANCED);
    let mut chase = fixture.completion(attempt, CANDIDATE, ADVANCED, HEAD_B);
    chase["chased_from"] = json!(HEAD_A);
    fixture
        .runtime
        .checkpoint_rebase_recovery(&fixture.run, STEP, &chase)
        .expect("a chase certifies as the step's newest attempt");
    assert!(fixture.verifies(&chase));
    assert!(!fixture.verifies(&recovery));

    let reserved = |runtime: &OrbitRuntime, step: &str| {
        runtime
            .rebase_recovery_attempts(&fixture.run, step)
            .unwrap()
            .into_iter()
            .map(|scope| (scope.head_sha_before, scope.target_base_sha))
            .collect::<Vec<_>>()
    };
    let expected = vec![
        (CANDIDATE.to_string(), PINNED.to_string()),
        (CANDIDATE.to_string(), ADVANCED.to_string()),
    ];
    assert_eq!(reserved(&fixture.runtime, STEP), expected);
    assert!(reserved(&fixture.runtime, "complete_pr").is_empty());
    let restarted =
        OrbitRuntime::from_roots(&fixture.global, &fixture.repo.join(".orbit")).unwrap();
    assert_eq!(reserved(&restarted, STEP), expected);
}

/// Fail every run-state write whose state names `marker`, as a crash or a full
/// disk would between certification and the run-store copy. Dropping the
/// returned connection's trigger restores the store.
fn fail_writes_naming(fixture: &Fixture, marker: &str) -> WriteFault {
    let db = orbit_config::resolved_audit_db_path(
        &orbit_common::Env::capture(),
        &orbit_config::ConfigRoots::new(&fixture.global, fixture.repo.join(".orbit")),
    )
    .unwrap();
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_recovery_copy \
             BEFORE UPDATE ON job_run_states \
             WHEN NEW.pipeline_state_json LIKE '%{marker}%' \
             BEGIN SELECT RAISE(ABORT, 'injected run-state write failure'); END; \
             CREATE TRIGGER fail_recovery_copy_insert \
             BEFORE INSERT ON job_run_states \
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
            .execute_batch(
                "DROP TRIGGER IF EXISTS fail_recovery_copy; \
                 DROP TRIGGER IF EXISTS fail_recovery_copy_insert;",
            )
            .unwrap();
    }
}

/// Delivery preparations authenticate unchanged candidates and the full fresh
/// observation through the same public host boundary as rewritten candidates.
fn fresh_preparation_certificates_bind_all_observations_across_restart_scenario() {
    let fixture = fixture();
    let certify = |runtime: &OrbitRuntime, step: &str, base: &str, extra: Value| {
        let attempt = runtime
            .begin_rebase_recovery_attempt(
                &fixture.run,
                step,
                &RebaseRecoveryAttemptScope {
                    workspace_path: fixture.workspace(),
                    head_sha_before: CANDIDATE.into(),
                    target_base_sha: base.into(),
                },
            )
            .unwrap();
        let mut payload = json!({
            "run_id":fixture.run, "step_id":step, "workspace_path":fixture.workspace(),
            "head_sha_before":CANDIDATE, "head_sha":CANDIDATE, "target_base_sha":base,
            "base_sha":base, "recovery_attempt":attempt, "task_ids":["T-REBASE"],
            "head":"candidate", "remote_sha":HEAD_A, "base_ref":"origin/agent-main",
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        runtime
            .checkpoint_rebase_recovery(&fixture.run, step, &payload)
            .unwrap();
        payload
    };
    let initial = certify(
        &fixture.runtime,
        "prepare_branch",
        PINNED,
        json!({"kind":"delivery_preparation"}),
    );
    let fresh = certify(
        &fixture.runtime,
        "sync_base_preparation",
        ADVANCED,
        json!({
            "kind":"delivery_preparation_refresh", "source_preparation":initial,
            "previous_base_sha":PINNED, "deadline_ms":121000,
            "original_error":"prepared base moved", "attempt_trace":[{"previous_base_sha":PINNED,"base_sha":ADVANCED}],
        }),
    );
    let restarted =
        OrbitRuntime::from_roots(&fixture.global, &fixture.repo.join(".orbit")).unwrap();
    assert!(
        restarted
            .verify_rebase_recovery(&fixture.run, "prepare_branch", &initial)
            .unwrap()
    );
    assert!(
        restarted
            .verify_rebase_recovery(&fixture.run, "sync_base_preparation", &fresh)
            .unwrap()
    );
    for field in [
        "run_id",
        "step_id",
        "task_ids",
        "workspace_path",
        "head",
        "head_sha",
        "remote_sha",
        "base_ref",
        "base_sha",
        "previous_base_sha",
        "source_preparation",
        "deadline_ms",
        "original_error",
        "attempt_trace",
    ] {
        let mut altered = fresh.clone();
        altered[field] = json!("foreign");
        let verified =
            restarted.verify_rebase_recovery(&fixture.run, "sync_base_preparation", &altered);
        assert!(
            matches!(verified, Ok(false)),
            "altered {field} must be rejected, not authenticated or errored: {verified:?}"
        );
    }
    assert_eq!(
        restarted
            .rebase_recovery_attempts(&fixture.run, "sync_base_preparation")
            .unwrap()
            .len(),
        1
    );
}

crate::scenario_table!(
    a_resumed_run_certifies_its_second_recovery_of_one_step,
    a_resumed_run_certifies_its_second_recovery_of_one_step_scenario
);
crate::scenario_table!(
    a_certified_attempt_whose_copy_failed_is_reissued_unchanged,
    a_certified_attempt_whose_copy_failed_is_reissued_unchanged_scenario
);
crate::scenario_table!(
    the_host_reports_each_reserved_attempt_and_certifies_a_chase_as_the_newest,
    the_host_reports_each_reserved_attempt_and_certifies_a_chase_as_the_newest_scenario
);
crate::scenario_table!(
    fresh_preparation_certificates_bind_all_observations_across_restart,
    fresh_preparation_certificates_bind_all_observations_across_restart_scenario
);

/// Databases created by builds before `authority.db` moved under `orbit-store`
/// (both pre-ledger `ensure_schema` files and v1 ledgered files) open cleanly
/// through the new driver and verify their persisted certificates without
/// rewriting existing records.
fn pre_change_authority_database_opens_and_verifies_certificates_scenario() {
    use orbit_core::runtime::recovery_authority::RecoveryAuthority;

    let base_checkpoint = |run_id: &str, head_sha: &str, attempt: Option<u64>| {
        let mut payload = json!({
            "run_id": run_id,
            "step_id": STEP,
            "task_ids": ["ORB-11977"],
            "workspace_path": "/fixture/workspace",
            "head": "orbit/ORB-11977",
            "head_sha_before": "1111111111111111111111111111111111111111",
            "original_base_sha": "2222222222222222222222222222222222222222",
            "base_ref": "refs/remotes/origin/agent-main",
            "target_base_sha": "3333333333333333333333333333333333333333",
            "base_sha": "3333333333333333333333333333333333333333",
            "remote_sha_before": Value::Null,
            "head_sha": head_sha,
            "rewritten": true,
        });
        if let Some(attempt) = attempt {
            payload["recovery_attempt"] = json!(attempt);
        }
        payload
    };

    // 1. Pre-ledger (`ensure_schema`, no `schema_meta`): adopts v1 and verifies
    //    both its pre-attempt legacy certificate and its attempt certificate.
    {
        let global = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let authority_dir = global.path().join("state/recovery-authority");
        std::fs::create_dir_all(&authority_dir).unwrap();
        let db_path = authority_dir.join(orbit_store::recovery_authority::FILE_NAME);
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute_batch(include_str!("../fixtures/pre_change_authority_v0.sql"))
            .unwrap();

        let authority = RecoveryAuthority::open(global.path()).unwrap();
        let legacy = base_checkpoint(
            "jrun-20261004-0001-1",
            "4444444444444444444444444444444444444444",
            None,
        );
        assert!(
            authority
                .verify("jrun-20261004-0001-1", STEP, &legacy)
                .unwrap()
        );
        let mut forged_legacy = legacy.clone();
        forged_legacy["head_sha"] = json!("9999999999999999999999999999999999999999");
        assert!(
            !authority
                .verify("jrun-20261004-0001-1", STEP, &forged_legacy)
                .unwrap()
        );

        let pre_ledger_attempt = base_checkpoint(
            "jrun-20261015-0002-1",
            "5555555555555555555555555555555555555555",
            Some(1),
        );
        assert!(
            authority
                .verify("jrun-20261015-0002-1", STEP, &pre_ledger_attempt)
                .unwrap()
        );
        let mut forged_attempt = pre_ledger_attempt.clone();
        forged_attempt["head_sha"] = json!("9999999999999999999999999999999999999999");
        assert!(
            !authority
                .verify("jrun-20261015-0002-1", STEP, &forged_attempt)
                .unwrap()
        );
        assert_eq!(
            authority.attempts("jrun-20261015-0002-1", STEP).unwrap(),
            vec![RebaseRecoveryAttemptScope {
                workspace_path: "/fixture/workspace".to_string(),
                head_sha_before: "1111111111111111111111111111111111111111".to_string(),
                target_base_sha: "3333333333333333333333333333333333333333".to_string(),
            }]
        );
        drop(authority);

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let baseline_name: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'migration.v0001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(baseline_name, "recovery_authority_baseline");
    }

    // 2. Pre-change ledgered v1: opens without rewriting `schema_meta` and
    //    verifies its persisted attempt certificate.
    {
        let global = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let authority_dir = global.path().join("state/recovery-authority");
        std::fs::create_dir_all(&authority_dir).unwrap();
        let db_path = authority_dir.join(orbit_store::recovery_authority::FILE_NAME);
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute_batch(include_str!("../fixtures/pre_change_authority_v1.sql"))
            .unwrap();

        let authority = RecoveryAuthority::open(global.path()).unwrap();
        let ledgered_attempt = base_checkpoint(
            "jrun-20261124-0003-1",
            "6666666666666666666666666666666666666666",
            Some(1),
        );
        assert!(
            authority
                .verify("jrun-20261124-0003-1", STEP, &ledgered_attempt)
                .unwrap()
        );
        let mut forged = ledgered_attempt.clone();
        forged["head_sha"] = json!("9999999999999999999999999999999999999999");
        assert!(
            !authority
                .verify("jrun-20261124-0003-1", STEP, &forged)
                .unwrap()
        );
        assert_eq!(
            authority.attempts("jrun-20261124-0003-1", STEP).unwrap(),
            vec![RebaseRecoveryAttemptScope {
                workspace_path: "/fixture/workspace".to_string(),
                head_sha_before: "1111111111111111111111111111111111111111".to_string(),
                target_base_sha: "3333333333333333333333333333333333333333".to_string(),
            }]
        );
        drop(authority);

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let applied_at: String = conn
            .query_row(
                "SELECT updated_at FROM schema_meta WHERE key = 'migration.v0001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(applied_at, "2026-11-24T00:00:00Z");
    }
}

crate::scenario_table!(
    pre_change_authority_database_opens_and_verifies_certificates,
    pre_change_authority_database_opens_and_verifies_certificates_scenario
);
