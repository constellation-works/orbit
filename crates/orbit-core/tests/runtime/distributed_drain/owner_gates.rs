//! A claimed leaf holds its candidate to the owner's validation gates, which
//! admission captures in the claim's receipt, not to the replica's own copy
//! [ORB-15622]. Per-host `.orbit/` configuration is untracked, so that copy
//! drifts silently: a replica lacking `review.baseline_commands` refused every
//! deferral of a gate the owner lists. A claim an older owner admitted
//! captured no gates, so the replica's configuration stands in for them, and
//! a drain records the owner's probe report so a difference is reported.
//!
//! The implementer's envelope lists the same captured gates as the commands
//! the pipeline's final validation runs, so a managed implementer leaves only
//! those to it [ORB-15963].

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
        // The owner captures its own `workflow.light_delivery_paths`, here
        // the default.
        light_delivery_paths: Some(
            orbit_types::task::DEFAULT_LIGHT_DELIVERY_PATHS
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect(),
        ),
    }
}

fn replica_gates() -> ValidationGates {
    ValidationGates {
        required_validation_commands: vec!["make replica-check".into()],
        baseline_commands: vec![],
        // A replica fallback carries its own list, which no leaf applies.
        light_delivery_paths: owner_gates().light_delivery_paths,
    }
}

/// The commands `host`'s implementer envelope for `task` leaves to final
/// validation, with or without the pipeline's `final_validation` input.
fn final_validation_commands(host: &OrbitRuntime, task: &str, final_validation: bool) -> Value {
    let input = json!({"task_id": task, "final_validation": final_validation});
    RuntimeHost::task_context_for_agent_input(host, "jrun-envelope", "implement", &input)
        .expect("the envelope builds")
        .expect("a task envelope")["final_validation_commands"]
        .clone()
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
        .derive(
            &summary,
            gates,
            Some(base),
            orbit_types::workflow::DeferralLane {
                claimed: true,
                os: orbit_types::workflow::EvidenceHostOs::current(),
            },
        )?
        .complete(vec![NamespaceProbe {
            command: BUBBLEWRAP_NAMESPACE_PROBE.to_string(),
            exit_code: 1,
            output: BUBBLEWRAP_NAMESPACE_REFUSAL.to_string(),
        }])
}

/// With review off, the ship contract captures no gates; the receipt alone
/// carries the owner's, and the leaf applies them over its own.
fn a_claimed_leaf_applies_the_owners_captured_gates_over_its_own() {
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

    // The envelope hands over exactly the captured list, partial as it is:
    // the baseline gate and every workspace check it omits stay with the
    // implementer, and nothing is handed over outside a pipeline that runs
    // final validation.
    assert_eq!(
        final_validation_commands(&leaf.bound, &leaf.task, true),
        json!(["make owner-check"]),
        "a claimed implementer leaves only the captured commands to `claim_validate`"
    );
    assert_eq!(
        final_validation_commands(&leaf.bound, &leaf.task, false),
        Value::Null
    );
    assert_eq!(
        final_validation_commands(&leaf.pair.wire.owner, &leaf.task, true),
        json!(["make owner-check"]),
        "an owner's implementer leaves its own configured commands to `candidate_validate`"
    );
    let no_diff = leaf.pair.wire.owner.run_tool("orbit.task.add", json!({
        "title": "Read-only envelope fixture", "description": "Inspect without a candidate change.",
        "acceptance_criteria": ["Inspection recorded"], "type": "chore", "tags": ["no-diff-expected"],
        "complexity": "low", "model": "codex",
        "workspace": leaf.pair.wire.owner.paths().repo_root,
    })).unwrap();
    assert_eq!(
        final_validation_commands(&leaf.pair.wire.owner, no_diff["id"].as_str().unwrap(), true),
        Value::Null,
        "a no-diff-expected invocation conveys no required-command ownership"
    );
}

/// A claim an older owner admitted captured no gates: the leaf falls back to
/// the replica's configuration, and a refusal says that is where its gate
/// list came from.
fn a_claim_without_captured_gates_falls_back_to_the_replica_config() {
    let leaf = ReviewedLeaf::admit_launched(OWNER_GATES, REPLICA_GATES, |pair, drain| {
        *pair.wire.gateless.lock().unwrap() = true;
        pair.launched_leaf(drain, 1, std::process::id())
    });
    assert_eq!(leaf.pair.admission(&leaf.leaf).receipt.unwrap().gates, None);

    let gates = RuntimeHost::validation_gates(&leaf.bound);
    assert_eq!(gates.source, ValidationGateSource::ReplicaConfig);
    assert_eq!(gates.gates, replica_gates());
    assert!(
        gates.light_delivery_paths().is_empty(),
        "a replica's own list never lets a claimed candidate skip review or validation"
    );
    let base = leaf.base.commit.as_str();
    let refusal = derive_deferral("make owner-affected", base, &gates).unwrap_err();
    assert!(
        refusal.contains("[`make replica-check`] from this replica's local configuration"),
        "{refusal}"
    );
    // The replica's list is not the owner's, so the implementer keeps every
    // check rather than leaving any to it.
    assert_eq!(
        final_validation_commands(&leaf.bound, &leaf.task, true),
        Value::Null
    );
}

/// Check every handover, searching later siblings and the continuations of
/// enclosing loops. A validator in a different loop is not a continuation:
/// that loop could run zero iterations or be skipped by its own condition.
fn gate_handovers(steps: &[Value], continuation: &[&Value]) -> Result<usize, String> {
    let mut count = 0;
    for (index, step) in steps.iter().enumerate() {
        let later: Vec<_> = steps[index + 1..]
            .iter()
            .chain(continuation.iter().copied())
            .collect();
        if matches!(
            step["target"].as_str(),
            Some("activity:agent_implement" | "activity:git_rebase")
        ) && step["default_input"]["final_validation"] == true
        {
            if step["target"] == "activity:git_rebase" && step["id"] != "sync_base" {
                return Err("only pre-publication recovery hands over gates".into());
            }
            let validates = later.iter().any(|validation| {
                if !matches!(
                    validation["target"].as_str(),
                    Some("activity:candidate_validate" | "activity:claim_validate")
                ) {
                    return false;
                }
                // No-diff-expected tasks receive no commands in their
                // implementer envelope (covered above), so this shipped
                // skip condition cannot drop handed-over commands. Recovery
                // still requires its exact condition, as before.
                let skips_only_no_diff = step["target"] == "activity:agent_implement"
                    && step["when"].is_null()
                    && validation["when"]
                        == "{{ steps.commit.output.skipped_no_diff_expected }} != true";
                validation["when"] == step["when"] || skips_only_no_diff
            });
            if !validates {
                return Err(format!(
                    "{} hands over required commands without a later validation continuation under the same condition",
                    step["id"]
                ));
            }
            count += 1;
        }
        if let Some(nested) = step["loop"]["steps"].as_array() {
            count += gate_handovers(nested, &later)?;
        }
    }
    Ok(count)
}

/// Required commands handed over by an implementer or recovery must have a
/// later validation continuation, including within a loop.
#[test]
fn every_pipeline_that_hands_over_its_gates_validates_after_implementing() {
    let jobs = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs");
    let mut handing_over = Vec::new();
    for entry in std::fs::read_dir(&jobs).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "yaml") {
            continue;
        }
        let job: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let steps = job["spec"]["steps"].as_array().unwrap();
        let count = gate_handovers(steps, &[])
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        if count != 0 {
            handing_over.push(path);
        }
    }
    assert!(
        !handing_over.is_empty(),
        "no delivery pipeline hands over its gates"
    );

    // Exercise the scan's decisions on job values, rather than matching YAML
    // text. These cases protect its condition, ordering and loop guarantees.
    let mut failures = Vec::new();
    for validation in ["activity:candidate_validate", "activity:claim_validate"] {
        let implement = json!({
            "id": "implement", "target": "activity:agent_implement",
            "when": "{{ input.repair }} == true",
            "default_input": {"final_validation": true},
        });
        let validate = json!({
            "id": "validate", "target": validation,
            "when": "{{ input.repair }} == true",
        });
        let mut different = validate.clone();
        different["when"] = json!("{{ input.repair }} == false");
        let cases = [
            ("matching condition", json!([implement, validate]), true),
            (
                "mutually exclusive condition",
                json!([implement, different]),
                false,
            ),
            (
                "same loop",
                json!([{"id": "bundle", "loop": {"steps": [implement, validate]}}]),
                true,
            ),
            (
                "different condition inside loop",
                json!([{"loop": {"steps": [implement, different]}}]),
                false,
            ),
            (
                "enclosing continuation",
                json!([{"loop": {"steps": [implement]}}, validate]),
                true,
            ),
            (
                "different condition outside loop",
                json!([{"loop": {"steps": [implement]}}, different]),
                false,
            ),
            (
                "nested loops",
                json!([{"loop": {"steps": [{"loop": {"steps": [implement, validate]}}]}}]),
                true,
            ),
            (
                "validation before implement",
                json!([validate, implement]),
                false,
            ),
            (
                "every implement needs validation",
                json!([implement, validate, implement]),
                false,
            ),
            (
                "unrelated loop cannot supply validation",
                json!([implement, {"loop": {"steps": [validate]}}]),
                false,
            ),
        ];
        for (name, steps, expected) in cases {
            let result = gate_handovers(steps.as_array().unwrap(), &[]);
            if result.is_ok() != expected {
                failures.push(format!(
                    "{validation}: {name}: expected {expected}, got {result:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A drain records the owner's probe report at submission, so `orbit run
/// auto --pull` and `orbit doctor` can name each list that differs, and say
/// so when the owner is too old to report any.
fn a_pull_drain_reports_how_the_replicas_gates_differ_from_the_owners() {
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_claimed_leaf_applies_the_owners_captured_gates_over_its_own_scenarios,
    a_claimed_leaf_applies_the_owners_captured_gates_over_its_own,
    a_claim_without_captured_gates_falls_back_to_the_replica_config,
    a_pull_drain_reports_how_the_replicas_gates_differ_from_the_owners
);
