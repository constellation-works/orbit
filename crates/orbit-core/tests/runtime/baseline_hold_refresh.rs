//! Moved-base refresh replays recorded failures before spending a full run.

use orbit_engine::{BaselineHoldStatus, baseline_hold_status, recorded_baseline_hold_status};
use orbit_types::workflow::{BaselineRedFailures, BaselineRedHold};
use serde_json::{Value, json};

use super::review_baseline_hold::{fixture, git};
use super::review_gate_audit::Fixture;

const GATE: &str = r#"#!/bin/sh
log="$(git rev-parse --path-format=absolute --git-common-dir)/refresh-runs.log"
selection="$ORBIT_VALIDATION_SELECTION"
[ -n "$selection" ] || selection='{"packages":["suite"]}'
echo "$selection" >> "$log"
tests=1
case "$selection" in
    *failing_tests*)
        mode=$(cat candidate.txt)
        case "$mode" in
            ignored) selection='{"packages":["suite"]}' ;;
            zero) tests=0 ;;
            inconclusive) exit 127 ;;
            no-summary) echo 'test suite::broken ... FAILED'; exit 1 ;;
        esac
        printf '{"schema_version":1,"selection":%s,"tests_run":%s}' "$selection" "$tests" > "$ORBIT_VALIDATION_SUMMARY"
        case "$mode" in
            red|ignored|zero) echo 'test suite::broken ... FAILED'; exit 1 ;;
            *) exit 0 ;;
        esac ;;
esac
printf '{"schema_version":1,"selection":%s,"tests_run":1}' "$selection" > "$ORBIT_VALIDATION_SUMMARY"
if grep -q full-red candidate.txt; then echo 'test suite::different ... FAILED'; exit 1; fi
exit 0
"#;

fn hold(fixture: &Fixture) -> BaselineRedHold {
    BaselineRedHold {
        base_ref: "main".into(),
        base_sha: git(&fixture.repo, &["rev-parse", "main"]),
        command: "sh check.sh".into(),
        run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
        selection: Some(json!({"packages": ["suite"]})),
        failures: BaselineRedFailures::new(["test suite::broken".into()], ""),
    }
}

fn move_base(fixture: &Fixture, mode: &str) {
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(fixture.repo.join("candidate.txt"), mode).unwrap();
    git(&fixture.repo, &["commit", "--quiet", "-am", mode]);
    git(&fixture.repo, &["checkout", "--quiet", "candidate"]);
}

fn runs(fixture: &Fixture) -> Vec<Value> {
    let common = git(
        &fixture.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    std::fs::read_to_string(std::path::Path::new(&common).join("refresh-runs.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn refresh(fixture: &Fixture, hold: &BaselineRedHold, lifted: bool) {
    let status = baseline_hold_status(&fixture.runtime, &fixture.repo, hold);
    assert_eq!(
        matches!(status, BaselineHoldStatus::Lifted(_)),
        lifted,
        "{status:?}"
    );
    assert_eq!(
        recorded_baseline_hold_status(&fixture.repo, hold),
        Some(status)
    );
}

fn recorded_failures_keep_the_hold_without_a_full_run_and_reuse_the_cache() {
    let fixture = fixture(GATE, "after\n");
    let hold = hold(&fixture);
    move_base(&fixture, "red");
    assert_eq!(recorded_baseline_hold_status(&fixture.repo, &hold), None);
    refresh(&fixture, &hold, false);
    refresh(&fixture, &hold, false);
    assert_eq!(
        runs(&fixture),
        vec![json!({
            "packages": ["suite"], "failing_tests": ["suite::broken"]
        })]
    );

    // A different failure on the full selection keeps the hold even after
    // the originally recorded test passes; only a full pass can release it.
    move_base(&fixture, "full-red");
    refresh(&fixture, &hold, false);
    move_base(&fixture, "green");
    refresh(&fixture, &hold, true);
    refresh(&fixture, &hold, true);
    let ran = runs(&fixture);
    assert_eq!(ran.len(), 5, "each tip runs once per selection");
    assert_eq!(ran[1], ran[0]);
    assert_eq!(ran[2], hold.selection.clone().unwrap());
    assert_eq!(&ran[3..], &ran[1..3]);
}

fn unusable_replays_fall_back_to_the_full_selection_before_lifting() {
    let fixture = fixture(GATE, "after\n");
    let hold = hold(&fixture);
    for mode in ["ignored", "zero", "inconclusive", "no-summary"] {
        move_base(&fixture, mode);
        refresh(&fixture, &hold, true);
        refresh(&fixture, &hold, true);
    }
    let ran = runs(&fixture);
    assert_eq!(ran.len(), 8);
    for pair in ran.as_chunks::<2>().0 {
        assert_eq!(pair[0]["failing_tests"], json!(["suite::broken"]));
        assert_eq!(pair[1], hold.selection.clone().unwrap());
    }
}

fn holds_without_runnable_recorded_tests_use_only_the_full_command() {
    let fixture = fixture(GATE, "after\n");
    let mut hold = hold(&fixture);
    for (mode, identities) in [
        ("old-hold", vec![]),
        (
            "build-failure",
            vec!["src/lib.rs:1:1: error: broken".into()],
        ),
        ("shortened-name", vec!["test suite::broken…".into()]),
    ] {
        hold.failures = BaselineRedFailures::new(identities, "");
        move_base(&fixture, mode);
        refresh(&fixture, &hold, true);
        refresh(&fixture, &hold, true);
    }
    hold.failures = BaselineRedFailures::new(["test suite::broken".into()], "");
    hold.selection = None;
    move_base(&fixture, "no-selection");
    refresh(&fixture, &hold, true);
    assert_eq!(runs(&fixture), vec![json!({"packages": ["suite"]}); 4]);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    recorded_failures_keep_the_hold_without_a_full_run_and_reuse_the_cache_scenarios,
    recorded_failures_keep_the_hold_without_a_full_run_and_reuse_the_cache,
    unusable_replays_fall_back_to_the_full_selection_before_lifting,
    holds_without_runnable_recorded_tests_use_only_the_full_command
);
