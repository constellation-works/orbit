//! A claimed leaf holds its candidate to the owner's validation gates, which
//! admission captures in the claim's receipt, not to the replica's own copy
//! [ORB-15622]. Per-host `.orbit/` configuration is untracked, so that copy
//! drifts silently: a replica lacking `review.baseline_commands` refused every
//! deferral of a gate the owner lists. A claim an older owner admitted
//! captured no gates, so the replica's configuration stands in for them, and
//! a drain records the owner's probe report so a difference is reported.

use orbit_types::workflow::{
    BUBBLEWRAP_NAMESPACE_PROBE, BUBBLEWRAP_NAMESPACE_REFUSAL, DeferredSandboxValidation,
    HostValidationGates, NamespaceProbe, ValidationGateSource, ValidationGates, deferral_handoff,
};

use super::claimed_review::ReviewedLeaf;
use super::single_pass::run_windowless_drain;
use super::*;

const OWNER_GATES: &str = "[workflow]\nrequired_validation_commands = [\"make owner-check\"]\n\
                           [review]\nbaseline_commands = [\"make owner-affected\"]\n";
const REPLICA_GATES: &str = "[workflow]\nrequired_validation_commands = [\"make replica-check\"]\n";

fn owner_gates() -> ValidationGates {
    ValidationGates {
        required_validation_commands: vec!["make owner-check".into()],
        baseline_commands: vec!["make owner-affected".into()],
    }
}

fn replica_gates() -> ValidationGates {
    ValidationGates {
        required_validation_commands: vec!["make replica-check".into()],
        baseline_commands: vec![],
    }
}

/// The validation summary a gate writes for a passing run of `command` on
/// `base`, whose only gap is a Bubblewrap deferral.
fn gate_summary(command: &str, base: &str) -> Value {
    json!({
        "schema_version": 1,
        "command": command, "exit_code": 0, "tests_run": 12, "base": base,
        "notices": [orbit_exec::bwrap_deferral_notice("spawn", BUBBLEWRAP_NAMESPACE_REFUSAL)],
    })
}

/// The record an implementer's Bubblewrap deferral of `command` derives under
/// `gates`: the summary's checks, then Orbit's own refused probe.
fn derive_deferral(
    command: &str,
    base: &str,
    gates: &HostValidationGates,
) -> Result<DeferredSandboxValidation, String> {
    let output = json!({"deferred_sandbox_validation": {"summary": ".orbit/tmp/gate.json"}});
    let handoff = deferral_handoff(&output)?.expect("the handoff names a summary");
    let summary = serde_json::to_vec(&gate_summary(command, base)).expect("summary encodes");
    handoff
        .derive(&summary, gates, Some(base))?
        .complete(NamespaceProbe {
            command: BUBBLEWRAP_NAMESPACE_PROBE.to_string(),
            exit_code: 1,
            output: BUBBLEWRAP_NAMESPACE_REFUSAL.to_string(),
        })
}

/// With review off, the ship contract captures no gates; the receipt alone
/// carries the owner's, and the leaf applies them over its own.
#[test]
fn a_claimed_leaf_applies_the_owners_captured_gates_over_its_own() {
    if !isolated(
        module_path!(),
        "a_claimed_leaf_applies_the_owners_captured_gates_over_its_own",
    ) {
        return;
    }
    let leaf = ReviewedLeaf::admit_from(OWNER_GATES, REPLICA_GATES);
    let receipt = leaf.pair.admission(&leaf.leaf).receipt.unwrap();
    assert!(receipt.request.ship.review.is_none(), "review is off");
    assert_eq!(receipt.gates, Some(owner_gates()));

    let gates = RuntimeHost::validation_gates(&leaf.bound);
    assert_eq!(gates.source, ValidationGateSource::OwnerContract);
    assert_eq!(gates.gates, owner_gates());
    assert_eq!(
        RuntimeHost::required_validation_commands(&leaf.bound),
        vec!["make owner-check".to_string()]
    );
    assert_eq!(
        RuntimeHost::review_baseline_commands(&leaf.bound),
        vec!["make owner-affected".to_string()]
    );
    let base = leaf.base.commit.as_str();
    let record = derive_deferral("make owner-affected", base, &gates)
        .expect("the owner's baseline gate is deferrable on a replica that lists none");
    assert_eq!(record.command, "make owner-affected");
    // The owner itself still validates its own runs against its own config.
    assert_eq!(
        RuntimeHost::validation_gates(&leaf.pair.wire.owner).source,
        ValidationGateSource::LocalConfig
    );
}

/// A claim an older owner admitted captured no gates: the leaf falls back to
/// the replica's configuration, and a refusal says that is where its gate
/// list came from.
#[test]
fn a_claim_without_captured_gates_falls_back_to_the_replica_config() {
    if !isolated(
        module_path!(),
        "a_claim_without_captured_gates_falls_back_to_the_replica_config",
    ) {
        return;
    }
    let leaf = ReviewedLeaf::admit_launched(OWNER_GATES, REPLICA_GATES, |pair, drain| {
        *pair.wire.gateless.lock().unwrap() = true;
        pair.launched_leaf(drain, 1, std::process::id())
    });
    assert_eq!(leaf.pair.admission(&leaf.leaf).receipt.unwrap().gates, None);

    let gates = RuntimeHost::validation_gates(&leaf.bound);
    assert_eq!(gates.source, ValidationGateSource::ReplicaConfig);
    assert_eq!(gates.gates, replica_gates());
    let base = leaf.base.commit.as_str();
    let refusal = derive_deferral("make owner-affected", base, &gates).unwrap_err();
    assert!(
        refusal.contains("[`make replica-check`] from this replica's local configuration"),
        "{refusal}"
    );
}

/// A drain records the owner's probe report at submission, so `orbit run
/// auto --pull` and `orbit doctor` can name each list that differs, and say
/// so when the owner is too old to report any.
#[test]
fn a_pull_drain_reports_how_the_replicas_gates_differ_from_the_owners() {
    if !isolated(
        module_path!(),
        "a_pull_drain_reports_how_the_replicas_gates_differ_from_the_owners",
    ) {
        return;
    }
    let drifted = Pair::with_configs(OWNER_GATES, REPLICA_GATES, &[]);
    let drain = run_windowless_drain(&drifted, 1, &[]);
    let drift = drifted.follower.pull_gate_drift(&drain).unwrap().unwrap();
    for expected in [
        "`workflow.required_validation_commands` is [`make replica-check`] here but \
         [`make owner-check`] on the owner",
        "`review.baseline_commands` is [] here but [`make owner-affected`] on the owner",
        "`.orbit/config.toml`",
    ] {
        assert!(drift.contains(expected), "missing {expected:?}: {drift}");
    }

    let matched = Pair::with_configs(OWNER_GATES, OWNER_GATES, &[]);
    let drain = run_windowless_drain(&matched, 1, &[]);
    assert_eq!(matched.follower.pull_gate_drift(&drain).unwrap(), None);

    let older = Pair::with_configs(OWNER_GATES, OWNER_GATES, &[]);
    *older.wire.gateless.lock().unwrap() = true;
    let drain = run_windowless_drain(&older, 1, &[]);
    let drift = older.follower.pull_gate_drift(&drain).unwrap().unwrap();
    assert!(drift.contains("reported no validation gates"), "{drift}");
}
