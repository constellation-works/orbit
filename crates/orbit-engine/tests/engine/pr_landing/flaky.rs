//! Candidate-only flake replay through both shipped validation actions.

use super::*;
use orbit_types::workflow::{FLAKY_VALIDATION_EVENT, FlakyValidation, MAX_FLAKY_TESTS};

const COMMAND: &str = "python3 test_gate.py";

// Selection and mode are deterministic fault injection; every process actually
// runs and writes its executed selection/count. No timing or synthetic load.
const GATE: &str = r#"import json, os
from pathlib import Path
mode = Path('mode').read_text().strip()
selection = json.loads(os.environ.get('ORBIT_VALIDATION_SELECTION', 'null'))
replay = selection is not None and 'failing_tests' in selection
with Path(__RUNS__).open('a') as runs:
    runs.write(json.dumps({'mode': mode, 'replay': replay, 'selection': selection}) + '\n')
if not replay:
    selection = {'packages': ['orbit-core'], 'target_flags': ['--lib', '--tests'], 'doctest_packages': []}
count = __COUNT__ if not replay else len(selection['failing_tests'])
failed = mode != 'base' and (not replay or mode == 'deterministic')
if mode == 'zero' and replay:
    count = 0
if mode == 'partial' and replay:
    count -= 1
if mode == 'mismatch' and replay:
    selection = dict(selection, packages=['other'])
Path(os.environ['ORBIT_VALIDATION_SUMMARY']).write_text(json.dumps({
    'schema_version': 1, 'selection': selection, 'tests_run': count
}))
if failed:
    for index in range(__COUNT__):
        print(f'FAIL [ 0.001s] (1/1) orbit-core::runtime suite::test_{index}')
    # libtest's echoed bare identity must not inflate the bound.
    print('test suite::test_0 ... FAILED')
else:
    print(f'PASS: {count} isolated tests')
    if mode == 'skip':
        print('SKIP: host fixture did not execute')
if mode == 'base' and __BASE_SKIP__:
    print('SKIP: base fixture did not execute')
raise SystemExit(100 if failed else 0)
"#;

fn fixture(sandbox: &Path, mode: &str, count: usize) -> (Fixture, PathBuf) {
    let fx = Fixture::new(sandbox);
    let runs = fx._root.path().join("gate-runs.jsonl");
    fx.commit_on_base(
        "test_gate.py",
        &GATE
            .replace("__RUNS__", &serde_json::to_string(&runs).unwrap())
            .replace("__COUNT__", &count.to_string())
            .replace(
                "__BASE_SKIP__",
                if mode == "base_skip" { "True" } else { "False" },
            ),
    );
    fx.commit_on_base("mode", "base\n");
    git(&fx.repo, &["rebase", BASE]);
    git(&fx.repo, &["push", "--force", "origin", BRANCH]);
    let candidate = fx.commit("mode", mode);
    (
        Fixture {
            base_sha: git(&fx.repo, &["rev-parse", BASE]),
            candidate,
            ..fx
        },
        runs,
    )
}

fn candidate_replay_rows(test: &str, route: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        for route in [route] {
            for (mode, count, succeeds, reruns) in [
                ("flake", MAX_FLAKY_TESTS, true, true),
                ("deterministic", 1, false, true),
                ("over_bound", MAX_FLAKY_TESTS + 1, false, false),
                ("base_skip", 1, false, false),
                ("zero", 1, false, true),
                ("partial", 2, false, true),
                ("mismatch", 1, false, true),
                ("skip", 1, false, true),
            ]
            .into_iter()
            .filter(|row| selected.contains(&row.0))
            {
                let (fx, runs) = fixture(sandbox, mode, count);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                host.require_commands(&[COMMAND]);
                if route == "claim_validate" {
                    *host.ship_mode.lock().unwrap() = "pr".into();
                }
                let input = if route == "candidate_validate" {
                    fx.validate_input()
                } else {
                    json!({"workspace_path": fx.repo, "base_sync": "local", "base_sha": fx.base_sha})
                };
                let outcome = action(&host, route, &input);
                let calls = fs::read_to_string(&runs)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(
                    calls.len(),
                    if reruns { 3 } else { 2 },
                    "{route}/{mode}: {calls:?}"
                );
                assert_eq!(calls[0]["mode"], mode);
                assert_eq!(calls[1]["mode"], "base");
                if reruns {
                    assert_eq!(calls[2]["mode"], mode);
                    assert_eq!(
                        calls[2]["selection"]["failing_tests"]
                            .as_array()
                            .unwrap()
                            .len(),
                        count
                    );
                }
                assert_eq!(fx.head(), fx.candidate);
                assert!(git(&fx.repo, &["status", "--porcelain"]).is_empty());
                if !succeeds {
                    let error = outcome
                        .expect_err("failed rerun or an over-bound set remains a failure")
                        .to_string();
                    assert!(!is_baseline_red_failure(None, Some(&error)), "{error}");
                    assert!(
                        host.history(TASK_ID)
                            .iter()
                            .all(|entry| entry.event != FLAKY_VALIDATION_EVENT)
                    );
                    let path = if route == "claim_validate" {
                        "validation/claim-landing/0.rerun.json"
                    } else {
                        "validation/jrun-landing/0.rerun.json"
                    };
                    assert_eq!(
                        if route == "claim_validate" {
                            host.claim_log_contents.lock().unwrap().contains_key(path)
                        } else {
                            host.has_artifact(TASK_ID, path)
                        },
                        reruns
                    );
                    continue;
                }
                let output = outcome.expect("all isolated tests passed");
                assert_eq!(output["decision"], "passed_with_flaky_tests");
                let activity = orbit_engine::activity_job::load_activity_asset(
                    &fs::read_to_string(
                        workspace_root()
                            .join("crates/orbit-core/assets/activities")
                            .join(format!("{route}.yaml")),
                    )
                    .unwrap(),
                )
                .unwrap()
                .spec;
                let schema = jsonschema::JSONSchema::compile(&activity.output_schema_json).unwrap();
                assert!(
                    schema.is_valid(&output),
                    "{route}: the shipped activity schema must accept the actual flaky result: {output}"
                );
                let records: Vec<FlakyValidation> =
                    serde_json::from_value(output["flaky"].clone()).unwrap();
                assert_eq!(records.len(), 1);
                let record = &records[0];
                assert!(record.valid());
                assert_eq!(record.rerun_tests_run, count as u64);
                assert_eq!(record.failures.len(), count);
                assert_eq!(record.original_exit_code, 100);
                if route == "candidate_validate" {
                    let history = host
                        .history(TASK_ID)
                        .into_iter()
                        .filter(|entry| entry.event == FLAKY_VALIDATION_EVENT)
                        .map(|entry| {
                            FlakyValidation::from_text(entry.note.as_deref().unwrap()).unwrap()
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(history, records);
                    let log = host.validation_log(TASK_ID, "validation/jrun-landing/0.json");
                    assert_eq!(log["failure_kind"], "flaky");
                    assert_eq!(log["exit_code"], 100, "the original failure is preserved");
                }
                let mut open = fx.open_input(&fx.candidate, &fx.base_sha);
                if route == "claim_validate" {
                    open["claim_validation"] = output.clone();
                }
                action(&host, "pr_open", &open).expect("publish validated candidate");
                let body = fx.forge_state("pr-body").unwrap();
                for test in &record.failures {
                    assert!(
                        body.contains(test.strip_prefix("test ").unwrap()),
                        "PR names the flaky tests: {body}"
                    );
                }
                assert!(
                    body.contains(&record.rerun_output),
                    "PR includes captured replay evidence"
                );
                if route == "claim_validate" {
                    let mut pin = input.clone();
                    pin["pull_request"] = json!(42);
                    pin["prevalidated"] = output.clone();
                    let pinned =
                        action(&host, "claim_validate", &pin).expect("pin without rerunning");
                    assert_eq!(pinned["flaky"], output["flaky"]);
                    let log: HandoffValidationLog = serde_json::from_slice(
                        &host.claim_log_contents.lock().unwrap()["validation/claim-landing/0.json"],
                    )
                    .unwrap();
                    assert_eq!(log.flaky, Some(record.clone()));
                    assert_eq!(log.exit_code, 0);
                    assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 3);
                }
            }
        }
    });
}

#[test]
fn candidate_validate_verdicts() {
    candidate_replay_rows(
        "flaky::candidate_validate_verdicts",
        "candidate_validate",
        &["flake", "deterministic", "over_bound", "base_skip"],
    );
}

#[test]
fn candidate_validate_coverage() {
    candidate_replay_rows(
        "flaky::candidate_validate_coverage",
        "candidate_validate",
        &["zero", "partial", "mismatch", "skip"],
    );
}

#[test]
fn claim_validate_verdicts() {
    candidate_replay_rows(
        "flaky::claim_validate_verdicts",
        "claim_validate",
        &["flake", "deterministic", "over_bound", "base_skip"],
    );
}

#[test]
fn claim_validate_coverage() {
    candidate_replay_rows(
        "flaky::claim_validate_coverage",
        "claim_validate",
        &["zero", "partial", "mismatch", "skip"],
    );
}
