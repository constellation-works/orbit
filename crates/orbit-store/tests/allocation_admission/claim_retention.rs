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
    fn mutate(
        &self,
        auth: &ClaimInvocation,
        mutation_id: &str,
        mutation: &ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        self.backends
            .commit_boundary
            .mutate_execution_claim(Some(auth), mutation_id, mutation)
    }

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
    if !isolated(
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
