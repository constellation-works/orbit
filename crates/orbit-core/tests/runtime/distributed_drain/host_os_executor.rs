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

/// The shipped muse executor, macOS only, as workspace init installs it.
fn declare_muse_macos(runtime: &OrbitRuntime) {
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: "muse".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some("muse".to_string()),
            args: Vec::new(),
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
    a_linux_follower_leaves_a_muse_crew_and_a_macos_follower_claims_it
);
