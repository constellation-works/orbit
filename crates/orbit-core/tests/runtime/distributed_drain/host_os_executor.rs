//! A muse crew follows the executor's `host_os` through owner pull admission.
//!
//! The follower's OS is composed, so the same pair withholds the task on
//! Linux and claims it on macOS.

use orbit_types::task::HostOs;

use super::*;

const CREWS: &str = r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"

[crews.muse-wonder]
model = "muse-spark-1.3"
provider = "muse"
"#;

const WAIT: &str = "waits for a macos host (crew muse-wonder)";

/// A Linux follower is not handed a muse-crew task. The idle receipt keeps
/// `host_os_mismatch` and the crew name. A macOS follower claims that same
/// task, still assigned to muse-wonder.
fn a_linux_follower_leaves_a_muse_crew_and_a_macos_follower_claims_it() {
    let mut pair = Pair::with_configs(CREWS, CREWS, &[Some("muse-wonder"), Some("sol")]);
    // The owner admits from its own executor store. Init would have seeded the
    // shipped muse asset; the follower's launch stand-in must not be what the
    // owner reads.
    declare_muse_macos(&pair.wire.owner);
    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Linux));
    let (muse, sol) = (pair.tasks[0].clone(), pair.tasks[1].clone());
    assert_eq!(pair.owner_task(&muse)["crew"], "muse-wonder");

    let drain = pair.start_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), sol);
    pair.leaf_fails_with(&leaf, "candidate validation failed");
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");
    assert_eq!(pair.owner_status(&muse), "backlog");
    assert!(
        !pair.owner_has_claim_for_task(&muse),
        "linux does not claim the muse task: {:#?}",
        pair.owner_claims()
    );
    assert_eq!(pair.owner_task(&muse)["crew"], "muse-wonder");

    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the idle pass is recorded");
    let waiting = recorded
        .excluded
        .iter()
        .find(|task| task.task_id == muse)
        .expect("the muse task is excluded");
    assert_eq!(waiting.reason.as_deref(), Some("host_os_mismatch"));
    assert!(
        waiting
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(WAIT)),
        "{waiting:?}"
    );
    let receipt = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .rev()
        .filter(|record| record.phase == LocalPullPhase::Idle)
        .find_map(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        receipt
            .os_unavailable
            .iter()
            .any(|skipped| { skipped.task_id == muse && skipped.reason.contains(WAIT) }),
        "{receipt:#?}"
    );

    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Macos));
    let mac_drain = pair.start_drain();
    let mac_leaf = pair.running_leaf(&mac_drain, 1);
    assert_eq!(pair.claimed_task(&mac_leaf), muse);
    assert_eq!(pair.owner_task(&muse)["crew"], "muse-wonder");
}

/// The only medium crew is macOS-only, so an owner's creation draw is
/// deterministic.
const POOL_CREWS: &str = r#"
[workflow]
default_crew = "sol"
medium_complexity_crews = ["muse-wonder"]

[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"

[crews.muse-wonder]
model = "muse-spark-1.3"
provider = "muse"
"#;

/// Move every claim lifecycle record in the owner's workspace eight days
/// into the past, in the owner's own store, as eight quiet days would
/// leave them.
fn age_claim_evidence(pair: &Pair) {
    let owner = &pair.wire.owner;
    let old = (Utc::now() - chrono::Duration::days(8)).to_rfc3339();
    let connection = rusqlite::Connection::open(owner.global_root().join("orbit.db")).unwrap();
    let moved = connection
        .execute(
            "UPDATE task_coordination_rows
             SET payload_json = json_set(payload_json, '$.created_at', ?1, '$.updated_at', ?1)
             WHERE workspace_id = ?2 AND kind = 'distributed-claim-lifecycle-v1'",
            rusqlite::params![old, owner.workspace_id().unwrap()],
        )
        .unwrap();
    assert!(moved >= 1, "the follower's claim lifecycle record aged");
}

/// A Linux owner's creation draw may pick the macOS-only pool crew only
/// once a registered macOS host actually serves this workspace. Before
/// the follower ever pulled here, registration alone — the follower's and
/// an unrelated Mac's alike — changes nothing. The follower's real claim
/// through owner pull admission is the evidence that counts; when that
/// evidence ages out of the window, draws fall back again.
fn a_recent_follower_claim_lets_a_linux_owner_draw_the_macos_only_crew() {
    let mut pair = Pair::with_configs(POOL_CREWS, POOL_CREWS, &[Some("sol")]);
    declare_muse_macos(&pair.wire.owner);
    // The owner as task creation on Linux composes it: this host is Linux,
    // and the host registry records the follower and an unrelated Mac.
    let linux_owner = pair
        .wire
        .owner
        .clone()
        .with_host_os(Some(HostOs::Linux))
        .with_registered_host_oses([
            (FOLLOWER.to_string(), HostOs::Macos),
            ("hm_unrelated".to_string(), HostOs::Macos),
        ]);
    let medium = |pair: &Pair, owner: &OrbitRuntime, file: &str| {
        backlog_task_with_input(
            owner,
            &pair.owner_repo,
            file,
            None,
            json!({"complexity": "medium"}),
        )
    };

    let before = medium(&pair, &linux_owner, "src/before.rs");
    assert_eq!(
        pair.owner_task(&before)["crew"],
        "sol",
        "registered Macs with no claim in this workspace change nothing"
    );

    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Macos));
    let drain = pair.start_drain();
    let _leaf = pair.running_leaf(&drain, 1);
    assert!(
        pair.owner_claims()
            .iter()
            .any(|claim| claim["claim"]["executed_on"]["machine_id"] == FOLLOWER),
        "the follower's claim is the owner's durable evidence: {:#?}",
        pair.owner_claims()
    );

    let drawn = medium(&pair, &linux_owner, "src/drawn.rs");
    let drawn_task = pair.owner_task(&drawn);
    assert_eq!(
        drawn_task["crew"], "muse-wonder",
        "a serving macOS follower lets the Linux owner draw the macOS-only crew"
    );
    assert_eq!(
        drawn_task["crew_source"], "pool:medium",
        "the draw records pool provenance"
    );

    age_claim_evidence(&pair);
    let aged = medium(&pair, &linux_owner, "src/aged.rs");
    assert_eq!(
        pair.owner_task(&aged)["crew"],
        "sol",
        "a follower whose last claim aged out no longer counts as serving"
    );
}

/// An explicitly constrained muse executor, macOS only, installed by this
/// fixture to exercise the general `host_os` contract.
fn declare_muse_macos(runtime: &OrbitRuntime) {
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: "muse".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some("muse".to_string()),
            args: Vec::new(),
            inspection_args: None,
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: None,
            allow_fallback: false,
            host_os: Some(BTreeSet::from([HostOs::Macos])),
            created_at: None,
            updated_at: None,
        })
        .expect("seed muse host_os");
}

crate::scenario_table!(
    a_linux_follower_leaves_a_muse_crew_and_a_macos_follower_claims_it_scenarios,
    a_linux_follower_leaves_a_muse_crew_and_a_macos_follower_claims_it,
    a_recent_follower_claim_lets_a_linux_owner_draw_the_macos_only_crew
);
