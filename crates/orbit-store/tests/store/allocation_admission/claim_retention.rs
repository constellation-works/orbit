//! Retention of settled claims' mutation receipts [ORB-15705].

use orbit_common::ClaimRefusalKind;
use orbit_store::contracts::FrictionAddParams;

use super::*;

const RECEIPT: &str = "distributed-claim-mutation-v1";
const FRICTION: &str = "distributed-claim-friction-v1";
const CLAIM: &str = "distributed-execution-claim-v1";
const STATE: &str = "distributed-claim-lifecycle-v1";

/// One admitted claim, the bind that started its leaf, and the leaf's
/// invocation.
struct Attempt {
    claim: ExecutionClaim,
    unbound: ClaimInvocation,
    bind: ClaimMutation,
    worker: ClaimInvocation,
}

impl Coordinated {
    /// Pull `request_id` and bind the claim to a leaf run of its own.
    fn bound_attempt(&self, request_id: &str) -> Attempt {
        let request = owner_request(request_id);
        let claim = self.pull(&request).claim.expect("claim");
        let run = ClaimRun {
            machine_id: claim.executed_on.machine_id.clone(),
            run_id: format!("leaf-{request_id}"),
        };
        let unbound = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id.clone(),
            None,
        );
        let bind = ClaimMutation::Bind {
            run: run.clone(),
            ship: request.ship,
        };
        self.mutate(&unbound, "bind", &bind).expect("bind");
        let worker = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id.clone(),
            Some(run),
        );
        Attempt {
            claim,
            unbound,
            bind,
            worker,
        }
    }
}

fn fail() -> ClaimMutation {
    ClaimMutation::Fail(ClaimEvidence {
        summary: Some("stopped".into()),
        ..Default::default()
    })
}

fn friction() -> ClaimMutation {
    ClaimMutation::Friction(FrictionAddParams {
        model: "codex".into(),
        title: None,
        body: "the leaf hit a gotcha".into(),
        tags: Vec::new(),
        status: Default::default(),
        during_task: None,
        created_at: Utc::now(),
    })
}

#[test]
fn friction_projection_follows_commit_and_silences_replays_and_rollbacks() {
    if !allocation_admission_isolated(
        "claim_retention::friction_projection_follows_commit_and_silences_replays_and_rollbacks",
    ) {
        return;
    }
    use orbit_common::observability::logging;

    // SAFETY: this exact test runs alone in its re-executed child, before
    // installing the subscriber and starting its background writer.
    unsafe { std::env::set_var("RUST_LOG", "warn") };
    logging::init_default_subscriber("warn");
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task_in("reporting", &["src/a.rs"]);
    let attempt = owner.bound_attempt("reporting");
    let token = format!("ghp_{}", "b".repeat(36));
    let params = FrictionAddParams {
        model: "gpt-6.1-sol".into(),
        title: Some(format!("Build exposed GITHUB_TOKEN={token}")),
        body: "Private claim evidence must stay out of live tracing".into(),
        tags: vec!["other".into()],
        status: Default::default(),
        during_task: None,
        created_at: Utc::now(),
    };
    let mutation = ClaimMutation::Friction(params.clone());
    let first = owner.mutate(&attempt.worker, "report", &mutation).unwrap();
    let record = first.friction.as_ref().unwrap();
    assert_eq!(
        record.during_task.as_deref(),
        Some(attempt.claim.task_id.as_str())
    );
    assert_eq!(
        owner.mutate(&attempt.worker, "report", &mutation).unwrap(),
        first
    );

    // Fail after allocation and insertion, when writing the replay row. A
    // projection emitted inside the transaction would falsely report success.
    let conn = sql(root.path());
    conn.execute_batch(
        "CREATE TRIGGER refuse_friction_receipt BEFORE INSERT ON task_coordination_rows
         WHEN NEW.kind = 'distributed-claim-friction-v1'
         BEGIN SELECT RAISE(ABORT, 'fixture refuses friction receipt'); END;",
    )
    .unwrap();
    let error = owner
        .mutate(&attempt.worker, "rolled-back", &mutation)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fixture refuses friction receipt")
    );
    conn.execute_batch("DROP TRIGGER refuse_friction_receipt")
        .unwrap();

    let store = compose::workspace_friction_store(
        Store::open(&root.path().join("state.sqlite")).unwrap(),
        PARTITION_ID,
        root.path().join("frictions"),
    )
    .unwrap();
    assert_eq!(store.list(&Default::default()).unwrap().len(), 1);
    assert_eq!(store.show(&record.id).unwrap().unwrap().record, *record);

    let mut automatic = params;
    automatic.body = "dedupe-key: report-once\nPrivate automation evidence".into();
    let created = store
        .add_or_reuse("report-once", automatic.clone())
        .unwrap();
    assert_eq!(
        store.add_or_reuse("report-once", automatic).unwrap().record,
        created.record
    );
    conn.execute_batch(
        "CREATE TRIGGER refuse_friction_insert BEFORE INSERT ON friction_records
         BEGIN SELECT RAISE(ABORT, 'fixture refuses friction insert'); END;",
    )
    .unwrap();
    let failed = FrictionAddParams {
        model: record.model.clone(),
        title: None,
        body: "Refused insert".into(),
        tags: vec!["other".into()],
        status: Default::default(),
        during_task: None,
        created_at: Utc::now(),
    };
    assert!(
        store
            .add(failed)
            .unwrap_err()
            .to_string()
            .contains("fixture refuses friction insert")
    );
    assert_eq!(store.list(&Default::default()).unwrap().len(), 2);

    logging::shutdown_jsonl_writer();
    let log = std::fs::read_to_string(logging::global_jsonl_log_path().unwrap()).unwrap();
    let events = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["target"] == "orbit.friction.reported")
        .collect::<Vec<_>>();
    assert_eq!(
        events.len(),
        2,
        "only newly committed friction records project"
    );
    for (stored, task_id) in [
        (record, attempt.claim.task_id.as_str()),
        (&created.record, ""),
    ] {
        let event = events
            .iter()
            .find(|event| event["fields"]["friction_id"] == stored.id)
            .unwrap();
        assert_eq!(event["fields"]["workspace_id"], PARTITION_ID);
        assert_eq!(event["fields"]["task_id"], task_id);
        assert_eq!(event["fields"]["agent"], "codex");
        assert_eq!(event["fields"]["model"], stored.model);
        assert!(
            event["fields"]["summary"]
                .as_str()
                .unwrap()
                .contains("[REDACTED_SECRET]")
        );
        assert!(event["fields"].get("body").is_none());
    }
    let projected = serde_json::to_string(&events).unwrap();
    assert!(!projected.contains(&token));
    assert!(!projected.contains(&record.body));
    assert!(!projected.contains(&created.record.body));
}

fn sql(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join("state.sqlite")).unwrap()
}

/// Receipt rows per claim id.
fn receipts_by_claim(root: &Path) -> BTreeMap<String, usize> {
    let conn = sql(root);
    let mut stmt = conn
        .prepare(
            "SELECT json_extract(json_extract(payload_json, '$.input'), '$[1]'), COUNT(*)
             FROM task_coordination_rows WHERE kind = ?1 GROUP BY 1",
        )
        .unwrap();
    stmt.query_map([RECEIPT], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn rows_of(root: &Path, kind: &str) -> usize {
    sql(root)
        .query_row(
            "SELECT COUNT(*) FROM task_coordination_rows WHERE kind = ?1",
            [kind],
            |row| row.get(0),
        )
        .unwrap()
}

/// Settled claims, a live claim and one recently settled: a settlement
/// deletes only the receipts (and friction replay rows) of claims settled
/// longer than the retention window ago. Claim and lifecycle rows all stay,
/// and a replay of a pruned mutation is refused `stale_claim` instead of
/// running again, while kept receipts still replay their recorded outcome.
#[test]
fn a_settlement_prunes_only_receipts_of_claims_settled_past_retention() {
    if !allocation_admission_isolated(
        "claim_retention::a_settlement_prunes_only_receipts_of_claims_settled_past_retention",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    for (title, file) in [
        ("old", "src/a.rs"),
        ("live", "src/b.rs"),
        ("recent", "src/c.rs"),
    ] {
        owner.create_task_in(title, &[file]);
    }

    let old = owner.bound_attempt("old");
    owner
        .mutate(&old.worker, "friction", &friction())
        .expect("friction");
    owner
        .mutate(&old.worker, "fail", &fail())
        .expect("fail old");
    let live = owner.bound_attempt("live");
    assert_eq!(rows_of(root.path(), FRICTION), 1);

    // Age every receipt and the old claim's last mutation past the window.
    let aged = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let conn = sql(root.path());
    conn.execute(
        "UPDATE task_coordination_rows SET created_at = ?1 WHERE kind IN (?2, ?3)",
        rusqlite::params![aged, RECEIPT, FRICTION],
    )
    .unwrap();
    conn.execute(
        "UPDATE task_coordination_rows SET payload_json = json_set(payload_json, '$.updated_at', ?1)
         WHERE kind = ?2 AND row_id = ?3",
        rusqlite::params![aged, STATE, old.claim.claim_id],
    )
    .unwrap();
    let before = receipts_by_claim(root.path());
    assert_eq!(
        before.get(&old.claim.claim_id),
        Some(&3),
        "bind, friction, fail"
    );
    assert_eq!(before.get(&live.claim.claim_id), Some(&1), "bind");

    // A later settlement is the automatic path.
    let recent = owner.bound_attempt("recent");
    owner
        .mutate(&recent.worker, "fail", &fail())
        .expect("fail recent");

    let after = receipts_by_claim(root.path());
    assert_eq!(
        after.get(&old.claim.claim_id),
        None,
        "a claim settled past retention loses its receipts"
    );
    assert_eq!(
        after.get(&live.claim.claim_id),
        Some(&1),
        "an unsettled claim keeps its receipts however old"
    );
    assert_eq!(
        after.get(&recent.claim.claim_id),
        Some(&2),
        "a claim settled inside the window keeps its receipts"
    );
    assert_eq!(
        rows_of(root.path(), FRICTION),
        0,
        "a pruned receipt's friction replay row goes with it"
    );
    assert_eq!(
        rows_of(root.path(), CLAIM),
        3,
        "claim rows are never pruned"
    );
    assert_eq!(
        rows_of(root.path(), STATE),
        3,
        "lifecycle rows are never pruned"
    );

    let replay = owner
        .mutate(&old.worker, "fail", &fail())
        .expect_err("a pruned mutation does not run again");
    assert_eq!(
        replay.claim_refusal(),
        Some(ClaimRefusalKind::StaleClaim),
        "a pruned mutation replays as stale_claim: {replay}"
    );
    assert_eq!(owner.task_status(&old.claim.task_id), TaskStatus::Blocked);
    let kept = owner
        .mutate(&recent.worker, "fail", &fail())
        .expect("a kept receipt replays");
    assert_eq!(kept.phase, ExecutionClaimPhase::Failed);
    let bound = owner
        .mutate(&live.unbound, "bind", &live.bind)
        .expect("a live claim's old receipt replays");
    assert_eq!(bound.phase, ExecutionClaimPhase::Running);
}
