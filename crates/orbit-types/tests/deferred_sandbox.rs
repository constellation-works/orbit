//! Deferred-sandbox records derived through the public workflow API
//! [ORB-15516]: the implementer names the gate's summary, and the record comes
//! from that summary, the lane the implementer ran in and Orbit's own probe.

// Test assertions unwrap results, following the test-only crate-root exemption.
#![allow(clippy::expect_used)]

use orbit_types::workflow::{
    BUBBLEWRAP_DEFERRAL_PREFIX, BUBBLEWRAP_NAMESPACE_PROBE, BUBBLEWRAP_NAMESPACE_REFUSAL,
    DeferralLane, DeferredSandboxValidation, EvidenceHostOs, HostValidationGates, NamespaceProbe,
    PS_DENIED_DEFERRAL_PREFIX, PS_EXEC_PROBE, SEATBELT_APPLY_PROBE, SEATBELT_DEFERRAL_PREFIX,
    ValidationGateSource, ValidationGates, deferral_handoff,
};
use serde_json::{Value, json};

const GATE: &str = "make ci-test-affected";
const BASE: &str = "e867385d9feda42090fdeb6fd83b8fa0c4d4eb4d";

fn bwrap_notice() -> String {
    format!("{BUBBLEWRAP_DEFERRAL_PREFIX} spawn_under_linux_bwrap: {BUBBLEWRAP_NAMESPACE_REFUSAL}")
}

fn seatbelt_notice() -> String {
    format!(
        "{SEATBELT_DEFERRAL_PREFIX} cli_ctrl_c_during_proc_spawn_reports_interrupt: permissive \
         profile exited with Some(71): sandbox-exec: sandbox_apply: Operation not permitted"
    )
}

fn ps_notice() -> String {
    format!(
        "{PS_DENIED_DEFERRAL_PREFIX} \
         run_observation::cancelling_owners_with_pre_change_ps_tokens_still_signals_them: the \
         sandbox denied running `ps`: Operation not permitted (os error 1)"
    )
}

/// An owner run on Linux, where every deferral but ps-denied is checked.
const OWNER_LINUX: DeferralLane = DeferralLane {
    claimed: false,
    os: Some(EvidenceHostOs::Linux),
};

/// A claimed leaf on macOS, the only lane that may defer ps-denied tests.
const CLAIMED_MACOS: DeferralLane = DeferralLane {
    claimed: true,
    os: Some(EvidenceHostOs::Macos),
};

/// The summary a passing gate wrote when its only gap is `notices`.
fn summary(notices: &[String]) -> Value {
    json!({
        "schema_version": 1,
        "selection": { "packages": ["orbit-engine"] },
        "tests_run": 42,
        "command": GATE,
        "base": BASE,
        "exit_code": 0,
        "notices": notices,
    })
}

fn bwrap_probe(exit_code: i32, output: &str) -> NamespaceProbe {
    NamespaceProbe {
        command: BUBBLEWRAP_NAMESPACE_PROBE.to_string(),
        exit_code,
        output: output.to_string(),
    }
}

fn seatbelt_probe(exit_code: i32, output: &str) -> NamespaceProbe {
    NamespaceProbe {
        command: SEATBELT_APPLY_PROBE.to_string(),
        exit_code,
        output: output.to_string(),
    }
}

fn ps_probe(exit_code: i32, output: &str) -> NamespaceProbe {
    NamespaceProbe {
        command: PS_EXEC_PROBE.to_string(),
        exit_code,
        output: output.to_string(),
    }
}

fn refused_ps_probe() -> NamespaceProbe {
    ps_probe(
        71,
        "sandbox-exec: execvp() of '/bin/ps' failed: Operation not permitted",
    )
}

fn refused_bwrap_probe() -> NamespaceProbe {
    bwrap_probe(1, &format!("bwrap: {BUBBLEWRAP_NAMESPACE_REFUSAL}"))
}

/// A host whose required validation commands are `commands`.
fn host_gates(commands: &[&str]) -> HostValidationGates {
    HostValidationGates::local(ValidationGates {
        required_validation_commands: commands.iter().map(ToString::to_string).collect(),
        baseline_commands: Vec::new(),
    })
}

/// Derive the record from `handoff`, the `summary` it names and `probe` the
/// way the implement step boundary does for an implementer in `lane`, after
/// checking which probe the summary asks Orbit to run.
fn derive_in(
    lane: DeferralLane,
    handoff: Value,
    summary: &Value,
    probe: NamespaceProbe,
) -> Result<DeferredSandboxValidation, String> {
    let output = json!({ "deferred_sandbox_validation": handoff });
    let handoff = deferral_handoff(&output)?.expect("the handoff names a summary");
    let pending = handoff.derive(
        &serde_json::to_vec(summary).expect("summary encodes"),
        &host_gates(&[GATE, "make goldens"]),
        Some(BASE),
        lane,
    )?;
    pending.complete(probe)
}

/// [`derive_in`] for an owner run on Linux.
fn derive(
    handoff: Value,
    summary: &Value,
    probe: NamespaceProbe,
) -> Result<DeferredSandboxValidation, String> {
    derive_in(OWNER_LINUX, handoff, summary, probe)
}

fn summary_handoff() -> Value {
    json!({ "summary": ".orbit/tmp/gate-summary.json" })
}

#[test]
fn a_bubblewrap_deferral_is_derived_from_the_summary_and_orbits_probe() {
    let notices = [bwrap_notice(), bwrap_notice()];

    let record = derive(summary_handoff(), &summary(&notices), refused_bwrap_probe())
        .expect("a passing gate with only Bubblewrap notices and a refused probe is deferrable");

    assert_eq!(
        record,
        DeferredSandboxValidation {
            command: GATE.to_string(),
            exit_code: 0,
            tests_run: 42,
            base: BASE.to_string(),
            notices: notices.to_vec(),
            probe: refused_bwrap_probe(),
        }
    );
}

/// Bubblewrap 0.9.0's refusal, verbatim from a claimed leaf on an Ubuntu
/// follower whose lane refuses nested user namespaces. Written out rather
/// than built from the constant, so the constant is held to the real text.
const BWRAP_0_9_REFUSAL: &str = "bwrap: No permissions to create new namespace, likely because \
     the kernel does not allow non-privileged user namespaces. See <https://deb.li/bubblewrap> \
     or <file:///usr/share/doc/bubblewrap/README.Debian.gz>.";

#[test]
fn bubblewraps_own_refusal_text_justifies_a_deferral() {
    let notices = [format!(
        "{BUBBLEWRAP_DEFERRAL_PREFIX} readonly_state_mount_keeps_cli_and_mcp_reads_observational: \
         bwrap exited exit status: 1: {BWRAP_0_9_REFUSAL}"
    )];

    let record = derive(
        summary_handoff(),
        &summary(&notices),
        bwrap_probe(1, BWRAP_0_9_REFUSAL),
    )
    .expect("a lane probe printing Bubblewrap's real refusal is deferrable");

    assert_eq!(record.probe.output, BWRAP_0_9_REFUSAL);
    assert_eq!(record.notices, notices.to_vec());
}

#[test]
fn the_summary_names_the_probe_of_the_sandbox_its_notices_defer() {
    let probe_of = |notice: String| {
        let output = json!({ "deferred_sandbox_validation": summary_handoff() });
        deferral_handoff(&output)
            .expect("well formed")
            .expect("present")
            .derive(
                &serde_json::to_vec(&summary(&[notice])).expect("encodes"),
                &host_gates(&[GATE]),
                Some(BASE),
                CLAIMED_MACOS,
            )
            .expect("deferrable before the probe")
            .probe()
    };

    let bubblewrap = probe_of(bwrap_notice());
    let seatbelt = probe_of(seatbelt_notice());
    let ps = probe_of(ps_notice());

    assert_eq!(bubblewrap.command, BUBBLEWRAP_NAMESPACE_PROBE);
    assert_eq!(bubblewrap.argv.join(" "), BUBBLEWRAP_NAMESPACE_PROBE);
    assert_eq!(seatbelt.command, SEATBELT_APPLY_PROBE);
    assert_eq!(seatbelt.argv[0], "/usr/bin/sandbox-exec");
    assert_eq!(ps.command, PS_EXEC_PROBE);
    assert_eq!(ps.argv.join(" "), PS_EXEC_PROBE);
}

/// [ORB-15587] A macOS claimed leaf whose agent sandbox refuses to exec `ps`
/// defers those tests to its claim host like a nested-sandbox skip. Every
/// other lane, mixed kind, unrefused probe or untyped skip is refused.
#[test]
fn a_ps_denied_deferral_is_derived_only_for_a_macos_claimed_leaf() {
    let notices = [ps_notice(), ps_notice()];

    let record = derive_in(
        CLAIMED_MACOS,
        summary_handoff(),
        &summary(&notices),
        refused_ps_probe(),
    )
    .expect("a macOS claimed leaf whose lane refuses `ps` defers its ps-denied tests");

    assert_eq!(record.notices, notices.to_vec());
    assert_eq!(record.probe, refused_ps_probe());

    let owner_macos = DeferralLane {
        claimed: false,
        os: Some(EvidenceHostOs::Macos),
    };
    let claimed_linux = DeferralLane {
        claimed: true,
        os: Some(EvidenceHostOs::Linux),
    };
    let claimed_unnamed = DeferralLane {
        claimed: true,
        os: None,
    };
    let cases = [
        (
            "an owner run on macOS",
            owner_macos,
            json!([ps_notice()]),
            refused_ps_probe(),
            "only a claimed leaf on macOS may defer; this implementer ran in its owner's run \
             on macos",
        ),
        (
            "a claimed leaf on Linux",
            claimed_linux,
            json!([ps_notice()]),
            refused_ps_probe(),
            "ran as a claimed leaf on linux",
        ),
        (
            "a claimed leaf on an unnamed OS",
            claimed_unnamed,
            json!([ps_notice()]),
            refused_ps_probe(),
            "ran as a claimed leaf on an unnamed OS",
        ),
        (
            "the untyped skip the test helper used to print",
            CLAIMED_MACOS,
            json!(["SKIP: the sandbox denied running `ps`: Operation not permitted (os error 1)"]),
            refused_ps_probe(),
            "is not a Bubblewrap, Seatbelt or ps-denied deferral",
        ),
        (
            "a Seatbelt notice beside a ps-denied one",
            CLAIMED_MACOS,
            json!([ps_notice(), seatbelt_notice()]),
            refused_ps_probe(),
            "is not a ps-denied deferral",
        ),
        (
            "a ps-denied notice beside a Bubblewrap one",
            CLAIMED_MACOS,
            json!([bwrap_notice(), ps_notice()]),
            refused_bwrap_probe(),
            "is not a Bubblewrap deferral",
        ),
        (
            "a lane that runs `ps`",
            CLAIMED_MACOS,
            json!([ps_notice()]),
            ps_probe(0, "    1"),
            "no refused `ps` probe",
        ),
        (
            "a probe that never ran",
            CLAIMED_MACOS,
            json!([ps_notice()]),
            ps_probe(
                -1,
                "the probe could not run: spawn: Operation not permitted (os error 1)",
            ),
            "no refused `ps` probe",
        ),
        (
            "a lane that cannot apply its sandbox at all",
            CLAIMED_MACOS,
            json!([ps_notice()]),
            ps_probe(71, "sandbox-exec: sandbox_apply: Operation not permitted"),
            "no refused `ps` probe",
        ),
        (
            "the Seatbelt probe's answer",
            CLAIMED_MACOS,
            json!([ps_notice()]),
            NamespaceProbe {
                command: SEATBELT_APPLY_PROBE.to_string(),
                exit_code: 71,
                output: "sandbox-exec: execvp() of '/bin/ps' failed: Operation not permitted"
                    .to_string(),
            },
            "names the probe",
        ),
    ];

    for (case, lane, notices, probe, reason) in cases {
        let mut summary = summary(&[]);
        summary["notices"] = notices;

        let refusal = derive_in(lane, summary_handoff(), &summary, probe).expect_err(case);

        assert!(refusal.contains(reason), "{case}: {refusal}");
    }
}

#[test]
fn a_seatbelt_deferral_needs_the_apply_refusal_exit() {
    let notices = [seatbelt_notice()];
    let refusal = "sandbox-exec: sandbox_apply: Operation not permitted";

    let record = derive(
        summary_handoff(),
        &summary(&notices),
        seatbelt_probe(71, refusal),
    )
    .expect("a refused Seatbelt apply probe backs Seatbelt notices");
    let failed_otherwise = derive(
        summary_handoff(),
        &summary(&notices),
        seatbelt_probe(1, refusal),
    )
    .expect_err("only exit 71 is the apply refusal");

    assert_eq!(record.probe.exit_code, 71);
    assert!(
        failed_otherwise.contains("no refused Seatbelt apply probe"),
        "{failed_otherwise}"
    );
}

#[test]
fn every_refusal_rule_holds_for_a_derived_record() {
    let with = |field: &str, value: Value| {
        let mut summary = summary(&[bwrap_notice()]);
        summary[field] = value;
        summary
    };
    let cases = [
        ("a nonzero exit", with("exit_code", json!(1)), "exited 1"),
        (
            "an unfinished gate",
            with("exit_code", Value::Null),
            "reports no exit status",
        ),
        (
            "zero tests",
            with("tests_run", json!(0)),
            "executed no tests",
        ),
        (
            "an uncounted run",
            with("tests_run", Value::Null),
            "executed no tests",
        ),
        (
            "another base",
            with("base", json!("0000000000000000000000000000000000000000")),
            "not the pinned delivery base",
        ),
        (
            "a selection rerun with no base",
            with("base", Value::Null),
            "not the pinned delivery base",
        ),
        (
            "a command no host replays",
            with("command", json!("scripts/codeql-rust-local.sh")),
            "not one of the owner's validation gates",
        ),
        (
            "a gate that records no command",
            with("command", Value::Null),
            "names no gate `command`",
        ),
        (
            "no notice",
            with("notices", json!([])),
            "lists no deferral notice",
        ),
        (
            "a SKIP notice beside a deferral",
            with(
                "notices",
                json!([bwrap_notice(), "SKIP: sandbox denying ps"]),
            ),
            "is not a Bubblewrap deferral",
        ),
        (
            "a notice of neither sandbox",
            with("notices", json!(["SKIP: sandbox denying ps"])),
            "not a Bubblewrap, Seatbelt or ps-denied deferral",
        ),
        (
            "both sandboxes' notices",
            with("notices", json!([bwrap_notice(), seatbelt_notice()])),
            "is not a Bubblewrap deferral",
        ),
        (
            "another summary schema",
            with("schema_version", json!(2)),
            "schema 2",
        ),
        (
            "a summary that is not a gate's",
            json!({ "selection": {} }),
            "is not a gate validation summary",
        ),
    ];

    for (case, summary, reason) in cases {
        let refusal = derive(summary_handoff(), &summary, refused_bwrap_probe()).expect_err(case);

        assert!(refusal.contains(reason), "{case}: {refusal}");
    }
}

#[test]
fn a_probe_that_shows_no_refusal_is_not_deferrable() {
    let cases = [
        (
            "a passing probe",
            bwrap_probe(0, ""),
            "no failing namespace probe",
        ),
        (
            "a probe that never reached the kernel",
            bwrap_probe(-1, "the probe could not run: No such file or directory"),
            "no failing namespace probe",
        ),
        (
            "the other sandbox's probe",
            seatbelt_probe(71, "sandbox-exec: sandbox_apply: Operation not permitted"),
            "names the probe",
        ),
    ];

    for (case, probe, reason) in cases {
        let refusal =
            derive(summary_handoff(), &summary(&[bwrap_notice()]), probe).expect_err(case);

        assert!(refusal.contains(reason), "{case}: {refusal}");
    }
}

#[test]
fn a_hand_written_field_is_accepted_only_when_it_equals_the_derivation() {
    let summary = summary(&[bwrap_notice()]);
    let handed = |field: &str, value: Value| {
        let mut handoff = summary_handoff();
        handoff[field] = value;
        handoff
    };

    let equal = derive(
        handed("tests_run", json!(42)),
        &summary,
        refused_bwrap_probe(),
    );
    let differs = derive(
        handed("tests_run", json!(41)),
        &summary,
        refused_bwrap_probe(),
    )
    .expect_err("a hand-written count that differs is refused");
    let unknown = derive(
        handed("coverage", json!("not_run")),
        &summary,
        refused_bwrap_probe(),
    )
    .expect_err("a key no record has is refused");

    assert_eq!(equal.expect("an equal field is accepted").tests_run, 42);
    assert!(differs.contains("hands `tests_run` as 41"), "{differs}");
    assert!(
        unknown.contains("unknown field `coverage`"),
        "guards the fail-closed extra-key refusal (F2026-10-261): {unknown}"
    );
}

#[test]
fn a_handoff_that_names_no_summary_is_refused_and_none_defers_nothing() {
    let hand_written = json!({
        "command": GATE,
        "exit_code": 0,
        "tests_run": 42,
        "base": BASE,
        "notices": [bwrap_notice()],
        "probe": { "command": BUBBLEWRAP_NAMESPACE_PROBE, "exit_code": 1, "output": BUBBLEWRAP_NAMESPACE_REFUSAL },
    });

    let refusal = deferral_handoff(&json!({ "deferred_sandbox_validation": hand_written }))
        .expect_err("a record with no summary cannot be derived");
    let absent = deferral_handoff(&json!({ "summary": "done" })).expect("no handoff is valid");
    let null = deferral_handoff(&json!({ "deferred_sandbox_validation": null }))
        .expect("a null handoff is valid");

    assert!(refusal.contains("names no gate summary"), "{refusal}");
    assert!(absent.is_none() && null.is_none());
}

/// [ORB-15622] A record naming a command outside the gates it is checked
/// against is refused with that list and where the host read it, so an
/// operator can tell a replica's drifted configuration from the owner's.
#[test]
fn a_record_naming_no_gate_is_refused_naming_the_gates_and_their_source() {
    let output = json!({ "deferred_sandbox_validation": summary_handoff() });
    let handoff = deferral_handoff(&output)
        .expect("well formed")
        .expect("present");
    for (source, named) in [
        (
            ValidationGateSource::OwnerContract,
            "captured in this claim's admission contract",
        ),
        (
            ValidationGateSource::ReplicaConfig,
            "this replica's local configuration",
        ),
    ] {
        let gates = HostValidationGates {
            gates: ValidationGates {
                required_validation_commands: vec!["make ci-fast".to_string()],
                baseline_commands: Vec::new(),
            },
            source,
        };

        let reason = handoff
            .derive(
                &serde_json::to_vec(&summary(&[bwrap_notice()])).expect("summary encodes"),
                &gates,
                Some(BASE),
                OWNER_LINUX,
            )
            .expect_err("a gate the host does not list is never deferrable");

        assert!(
            reason.contains(&format!("names `{GATE}`"))
                && reason.contains("the gates checked are [`make ci-fast`]")
                && reason.contains(named),
            "{source:?}: {reason}"
        );
    }
}
