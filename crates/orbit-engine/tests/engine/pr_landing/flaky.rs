//! Candidate-only flake replay through both shipped validation actions.

use super::*;
use orbit_types::workflow::{FLAKY_VALIDATION_EVENT, FlakyValidation, MAX_FLAKY_TESTS};

const COMMAND: &str = "python3 test_gate.py";

// Selection and mode are deterministic fault injection; every process actually
// runs and writes its executed selection/count. No timing or synthetic load.
const GATE: &str = r#"import json, os
from pathlib import Path
mode = Path('mode').read_text().strip()
base_mode = __MODE__
selection = json.loads(os.environ.get('ORBIT_VALIDATION_SELECTION', 'null'))
replay = selection is not None and 'failing_tests' in selection
with Path(__RUNS__).open('a') as runs:
    runs.write(json.dumps({'mode': mode, 'replay': replay, 'selection': selection}) + '\n')
if not replay:
    selection = {'packages': ['orbit-core'], 'target_flags': ['--lib', '--tests'], 'doctest_packages': []}
count = __COUNT__ if not replay else len(selection['failing_tests'])
executed = [f'orbit-core::runtime suite::test_{index}' for index in range(count)]
failed = mode != 'base' and (not replay or mode == 'deterministic')
if mode == 'base' and base_mode in ('inherited', 'subset', 'cache_old', 'cache_malformed', 'cache_partial', 'full_cache'):
    failed = True
if mode == 'zero' and replay or mode == 'base' and base_mode == 'base_zero':
    count = 0
    executed = []
if mode == 'partial' and replay or mode == 'base' and base_mode == 'base_partial':
    count -= 1
    executed = executed[:-1]
if mode == 'base' and base_mode == 'base_missing':
    executed = None
if mode == 'base' and base_mode == 'base_malformed':
    executed = ['not a test identity']
if mode == 'base' and base_mode == 'base_wrong':
    # Plausible count, wrong third identity: counts cannot prove coverage.
    executed[-1] = 'orbit-core::other suite::test_2'
if mode == 'base' and base_mode == 'base_compile':
    count, executed, failed = None, None, True
if mode == 'mismatch' and replay:
    selection = dict(selection, packages=['other'])
Path(os.environ['ORBIT_VALIDATION_SUMMARY']).write_text(json.dumps({
    'schema_version': 1, 'selection': selection, 'tests_run': count, 'executed_tests': executed
}))
if failed:
    for index in range(2 if mode == 'base' and base_mode == 'subset' else __COUNT__):
        print(f'FAIL [ 0.001s] (1/1) orbit-core::runtime suite::test_{index}')
    # libtest's echoed bare identity must not inflate the bound.
    print('test suite::test_0 ... FAILED')
    if mode == 'base' and base_mode == 'base_compile':
        print('error: compile failed\n  --> src/lib.rs:1:1')
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
    let gate = GATE
        .replace("__MODE__", &serde_json::to_string(mode).unwrap())
        .replace("__RUNS__", &serde_json::to_string(&runs).unwrap())
        .replace("__COUNT__", &count.to_string())
        .replace(
            "__BASE_SKIP__",
            if mode == "base_skip" { "True" } else { "False" },
        );
    fx.commit_files_on_base(&[("test_gate.py", &gate), ("mode", "base\n")]);
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

#[test]
fn pr_open_keeps_latest_matching_flaky_history() {
    isolated(
        "flaky::pr_open_keeps_latest_matching_flaky_history",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let mut expected = Vec::new();
            let mut handed = Vec::new();
            for mismatch in ["run", "base", "head"] {
                let record = FlakyValidation {
                    command: format!("check-{mismatch}"),
                    base_sha: fx.base_sha.clone(),
                    tested_head: fx.candidate.clone(),
                    run_id: RUN_ID.into(),
                    failures: BTreeSet::from([format!("test suite::{mismatch}")]),
                    selection: json!({"failing_tests": [format!("suite::{mismatch}")]}),
                    original_exit_code: 1,
                    original_output: "original failure".into(),
                    base_output: "base passed".into(),
                    rerun_output: format!("matching replay for {mismatch}"),
                    rerun_tests_run: 1,
                    validation_env: json!({}),
                };
                assert!(record.valid());
                let mut newer = record.clone();
                match mismatch {
                    "run" => newer.run_id = "another-run".into(),
                    "base" => newer.base_sha = "another-base".into(),
                    "head" => newer.tested_head = "another-head".into(),
                    _ => unreachable!(),
                }
                newer.rerun_output = format!("unrelated replay for {mismatch}");
                host.history
                    .lock()
                    .unwrap()
                    .entry(TASK_ID.into())
                    .or_default()
                    .extend([&record, &newer].map(|record| TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: FLAKY_VALIDATION_EVENT.into(),
                        note: Some(record.text()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }));
                expected.push(record);
                handed.push(newer);
            }
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            input["claim_validation"] = json!({"flaky": handed});
            action(&host, "pr_open", &input).expect("publish candidate with matching evidence");
            let body = fx.forge_state("pr-body").unwrap();
            for record in expected {
                assert!(
                    body.contains(&record.rerun_output),
                    "matching history survives newer unrelated history and handed evidence: {body}"
                );
            }
            for record in handed {
                assert!(
                    !body.contains(&record.rerun_output),
                    "unrelated evidence is excluded: {body}"
                );
            }
        },
    );
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
                ("inherited", 3, true, false),
                ("subset", 3, false, false),
                ("base_zero", 3, false, false),
                ("base_partial", 3, false, false),
                ("base_missing", 3, false, false),
                ("base_malformed", 3, false, false),
                ("base_wrong", 3, false, false),
                ("base_compile", 3, false, false),
                ("cache_old", 3, true, false),
                ("cache_malformed", 3, true, false),
                ("cache_partial", 3, true, false),
                ("full_cache", 3, true, false),
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
                assert_eq!(
                    calls[0]["replay"], false,
                    "the required candidate gate stays full"
                );
                assert_eq!(calls[1]["mode"], "base");
                assert_eq!(calls[1]["replay"], true, "one exact diagnostic base replay");
                assert_eq!(
                    calls[1]["selection"]["failing_tests"]
                        .as_array()
                        .unwrap()
                        .len(),
                    count
                );
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
                    assert!(!error.contains("candidate introduced"), "{error}");
                    if mode == "over_bound" {
                        assert!(
                            error.contains("failure was not reproduced")
                                && error.contains("replayed only the candidate's failing tests")
                                && error.contains("full-suite or load equivalence")
                                && !error.contains("not comparable"),
                            "[F2026-10-377] supported exact replay proves nonreproduction within its narrower scope: {error}"
                        );
                    }
                    if mode.starts_with("base_") && !matches!(mode, "base_skip" | "base_compile") {
                        assert!(
                            error.contains("not comparable")
                                && error.contains("source causation is unconfirmed"),
                            "incomplete exact replay evidence remains uncertain: {error}"
                        );
                    }
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
                if matches!(
                    mode,
                    "inherited" | "cache_old" | "cache_malformed" | "cache_partial" | "full_cache"
                ) {
                    assert_eq!(output["decision"], "passed_with_known_red");
                    if mode != "inherited" {
                        let common = orbit_common::fs::git::git_common_dir(&fx.repo).unwrap();
                        let cache_dir = common.join("orbit-baseline");
                        let path = fs::read_dir(&cache_dir)
                            .unwrap()
                            .map(|entry| entry.unwrap().path())
                            .find(|path| {
                                path.extension()
                                    .is_some_and(|extension| extension == "json")
                            })
                            .unwrap();
                        let mut cached: Value =
                            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                        let destination = if mode == "full_cache" {
                            // A compatible full cache predating identity proof
                            // remains authoritative and avoids a base process.
                            cached["summary"]["selection"]
                                .as_object_mut()
                                .unwrap()
                                .remove("failing_tests");
                            cached["summary"]
                                .as_object_mut()
                                .unwrap()
                                .remove("executed_tests");
                            let key = orbit_common::security::release::sha256_hex(
                                format!(
                                    "{}\0{}\0{}\0{}",
                                    cached["schema_version"],
                                    fx.base_sha,
                                    COMMAND,
                                    cached["summary"]["selection"]
                                )
                                .as_bytes(),
                            );
                            cache_dir.join(format!("{key}.json"))
                        } else {
                            match mode {
                                "cache_old" => {
                                    cached["summary"]
                                        .as_object_mut()
                                        .unwrap()
                                        .remove("executed_tests");
                                }
                                "cache_malformed" => {
                                    cached["summary"]["executed_tests"] = json!([42]);
                                }
                                "cache_partial" => {
                                    cached["summary"]["executed_tests"]
                                        .as_array_mut()
                                        .unwrap()
                                        .pop();
                                }
                                _ => unreachable!(),
                            }
                            path
                        };
                        fs::write(destination, serde_json::to_vec(&cached).unwrap()).unwrap();
                        assert_eq!(
                            action(&host, route, &input).unwrap()["decision"],
                            "passed_with_known_red"
                        );
                        assert_eq!(
                            fs::read_to_string(&runs).unwrap().lines().count(),
                            if mode == "full_cache" { 3 } else { 4 },
                            "compatible full cache skips base; invalid narrow proof reruns it"
                        );
                    }
                    continue;
                }
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
        &[
            "flake",
            "deterministic",
            "over_bound",
            "base_skip",
            "inherited",
            "subset",
            "cache_old",
            "cache_malformed",
            "cache_partial",
            "full_cache",
        ],
    );
}

#[test]
fn candidate_validate_coverage() {
    candidate_replay_rows(
        "flaky::candidate_validate_coverage",
        "candidate_validate",
        &[
            "zero",
            "partial",
            "mismatch",
            "skip",
            "base_zero",
            "base_partial",
            "base_missing",
            "base_malformed",
            "base_wrong",
            "base_compile",
        ],
    );
}

#[test]
fn claim_validate_verdicts() {
    candidate_replay_rows(
        "flaky::claim_validate_verdicts",
        "claim_validate",
        &[
            "flake",
            "deterministic",
            "over_bound",
            "base_skip",
            "inherited",
            "subset",
            "cache_old",
            "cache_malformed",
            "cache_partial",
            "full_cache",
        ],
    );
}

#[test]
fn claim_validate_coverage() {
    candidate_replay_rows(
        "flaky::claim_validate_coverage",
        "claim_validate",
        &[
            "zero",
            "partial",
            "mismatch",
            "skip",
            "base_zero",
            "base_partial",
            "base_missing",
            "base_malformed",
            "base_wrong",
            "base_compile",
        ],
    );
}
