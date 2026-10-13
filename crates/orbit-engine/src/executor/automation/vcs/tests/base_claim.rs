#[cfg(unix)]
use std::fs;
use std::path::Path;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use orbit_common::test_env::canonical_temp_dir;
#[cfg(unix)]
use serde_json::json;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::super::base_claim::failure_identities;
#[cfg(unix)]
use super::super::base_claim::{BaseFailureVerdict, BaseReplay, ReplayBudget, verify_base_failure};
#[cfg(unix)]
use super::super::baseline::{cached_base_check, known_red};
#[cfg(unix)]
use super::super::required_command::{VALIDATION_TIMEOUT_MS, run_required_command};
#[cfg(unix)]
#[cfg(unix)]
struct Host;

#[cfg(unix)]
impl crate::test_support::NullRuntimeHost for Host {}

// Fault injection [ORB-15590]: a small capture ceiling forces a genuine
// supervisor stop without generating large output or load. Its process-wide
// environment needs a child; the settlement boundary covers a real signal.
#[test]
#[cfg(unix)]
fn supervisor_stops_never_classify_partial_failures_as_known_red() {
    const MARKER: &str = "ORBIT_TEST_BASE_CLAIM_STOP_CHILD";
    let test = concat!(
        module_path!(),
        "::supervisor_stops_never_classify_partial_failures_as_known_red"
    )
    .split_once("::")
    .unwrap()
    .1;
    if std::env::var(MARKER).as_deref() != Ok(test) {
        let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(MARKER, test)
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .current_dir(root.path());
        let output = orbit_common::test_env::run_child_test(&mut child, test, root.path());
        orbit_common::test_env::assert_child_test_passed(
            test,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }

    for (case, stop, cause) in [
        (
            "signal",
            "kill -TERM $$",
            Some("process terminated with signal:"),
        ),
        (
            "capture",
            "printf '%2048s' ''",
            Some("process output capture limit exceeded on stdout"),
        ),
        ("completed", ":", None),
    ] {
        let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        fs::write(
            repo.join("check.sh"),
            format!(
                "#!/bin/sh\n\
                 selection=\"$ORBIT_VALIDATION_SELECTION\"\n\
                 [ -n \"$selection\" ] || selection='{{\"packages\":[\"suite\"]}}'\n\
                 printf '{{\"schema_version\":1,\"selection\":%s,\"tests_run\":1}}' \"$selection\" \
                 > \"$ORBIT_VALIDATION_SUMMARY\"\n\
                 echo 'test suite::broken ... FAILED'\n\
                 if [ -f candidate.txt ]; then {stop}; fi\n\
                 exit 1\n"
            ),
        )
        .unwrap();
        git_ok(&repo, &["add", "check.sh"]);
        git_ok(&repo, &["commit", "-q", "-m", "base"]);
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        fs::write(repo.join("candidate.txt"), "after\n").unwrap();
        git_ok(&repo, &["add", "candidate.txt"]);
        git_ok(&repo, &["commit", "-q", "-m", "candidate"]);

        let _env = orbit_common::test_env::scoped([(
            "ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES",
            Some("1024"),
        )]);
        let command = "exec sh check.sh";
        let failures = ["test suite::broken".to_string()].into_iter().collect();
        let run = run_required_command(&Host, &repo, command, None, VALIDATION_TIMEOUT_MS).unwrap();
        assert!(!run.timed_out && !run.passed, "{case}: a failed, uncut run");
        assert_eq!(run.failures, failures, "{case}: printed failures survive");
        assert_eq!(
            known_red(&run, &failures),
            cause.is_none(),
            "[ORB-15590] the shared predicate refuses supervisor-stopped partial failures: {case}"
        );

        let check = verify_base_failure(
            &Host,
            &repo,
            base.trim(),
            command,
            &["suite::broken".to_string()],
            "jrun-fixture",
            ReplayBudget::new(Duration::from_secs(60)),
        )
        .unwrap();
        assert_eq!(check.cut, None, "{case}: not a reserve cut");
        match cause {
            Some(cause) => {
                assert!(
                    run.infrastructure_failure
                        .as_deref()
                        .unwrap()
                        .starts_with(cause),
                    "{case}: the supervisor supplied the stop cause"
                );
                assert!(
                    matches!(&check.verdict, BaseFailureVerdict::Inconclusive(reason) if reason.contains(cause)),
                    "[ORB-15590] {case}: partial failures stay inconclusive: {:?}",
                    check.verdict
                );
                assert_eq!(
                    check.base_replay, None,
                    "{case}: no replay of partial failures"
                );
                assert_eq!(
                    check.base_log,
                    serde_json::Value::Null,
                    "{case}: no base run"
                );
            }
            None => {
                assert_eq!(
                    check.verdict,
                    BaseFailureVerdict::KnownRed {
                        failures: failures.into_iter().collect()
                    }
                );
                assert_eq!(check.base_replay, Some(BaseReplay::FailingTests));
                let mut baseline = cached_base_check(
                    &repo,
                    base.trim(),
                    command,
                    check.base_log["summary"].get("selection"),
                )
                .unwrap();
                let mut run = run;
                assert!(baseline.reproduces(&run));
                for (candidate_timeout, base_timeout) in
                    [(true, false), (false, true), (true, true)]
                {
                    run.timed_out = candidate_timeout;
                    baseline.result.as_mut().unwrap().timed_out = base_timeout;
                    assert!(
                        !baseline.reproduces(&run),
                        "timeouts cannot confirm matching failures"
                    );
                }
            }
        }
    }
}

#[cfg(unix)]
fn git_ok(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(repo);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// The output grammars of each runner meet the combinatorial pure-logic
// unit-test criterion: the boundary test drives one of them.
#[test]
fn failure_identities_name_each_failed_test_and_located_error_across_runners() {
    let root = Path::new("/work/run-a");
    let output = "\
running 3 tests
test suite::ok ... ok
test suite::broken ... FAILED
\u{1b}[31m        FAIL\u{1b}[0m [   0.004s] (2/9) orbit-core runtime::red
     TIMEOUT [  60.001s] orbit-core runtime::slow
       PASS [   0.002s] orbit-core runtime::fine
error[E0308]: mismatched types
  --> /work/run-a/src/lib.rs:4:9
warning: unused variable
  --> src/lib.rs:7:1
error: an error with no location
error: this `if` has identical blocks
   --> src/main.rs:10:5
";
    let found = failure_identities(output, root);
    assert_eq!(
        found.into_iter().collect::<Vec<_>>(),
        vec![
            "src/lib.rs:4:9: error[E0308]: mismatched types",
            "src/main.rs:10:5: error: this `if` has identical blocks",
            "test orbit-core runtime::red",
            "test orbit-core runtime::slow",
            "test suite::broken",
        ],
        "an error is located under its own root; warnings and passes are not failures"
    );
    // The same failures from another checkout are the same identities.
    assert_eq!(
        failure_identities(
            &output.replace("/work/run-a", "/base/tree"),
            Path::new("/base/tree")
        ),
        failure_identities(output, root)
    );
}

// Fault injection: the candidate's check prints a failure and then stalls, so
// only the budget's reserve stops it. A real budget is minutes of review time;
// this one is scaled down to seconds, keeping the production relation (the
// candidate stops a quarter of the window early). The boundary test drives
// the uncut paths [ORB-15328].
#[test]
#[cfg(unix)]
fn a_candidate_rerun_stopped_at_the_reserve_still_attributes_its_failures_to_the_base() {
    let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    // Reports the selection it is handed (its own otherwise) before it runs,
    // fails `suite::broken` everywhere, and stalls only on the candidate.
    fs::write(
        repo.join("check.sh"),
        "#!/bin/sh\n\
         selection=\"$ORBIT_VALIDATION_SELECTION\"\n\
         [ -n \"$selection\" ] || selection='{\"packages\":[\"suite\"]}'\n\
         printf '{\"schema_version\":1,\"selection\":%s,\"tests_run\":1}' \"$selection\" \
         > \"$ORBIT_VALIDATION_SUMMARY\"\n\
         echo 'test suite::broken ... FAILED'\n\
         if [ -f candidate.txt ]; then exec sleep 600; fi\n\
         exit 1\n",
    )
    .unwrap();
    fs::set_permissions(repo.join("check.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    git_ok(&repo, &["add", "check.sh"]);
    git_ok(&repo, &["commit", "-q", "-m", "base"]);
    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    fs::write(repo.join("candidate.txt"), "after\n").unwrap();
    git_ok(&repo, &["add", "candidate.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "candidate"]);

    let budget = ReplayBudget::new(Duration::from_secs(4));
    let started = Instant::now();
    let check = verify_base_failure(
        &Host,
        &repo,
        base.trim(),
        "./check.sh",
        &["suite::broken".to_string()],
        "jrun-fixture",
        budget,
    )
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the stalled candidate run was stopped"
    );
    assert_eq!(
        check.verdict,
        BaseFailureVerdict::Cut {
            reason: check.cut.clone().expect("the check names the cut"),
            failures: vec!["test suite::broken".to_string()],
        },
        "the failure the cut run printed is attributed to the base, and nothing is confirmed"
    );
    assert_eq!(check.base_replay, Some(BaseReplay::FailingTests));
    assert_eq!(
        check.base_log["summary"]["selection"],
        json!({"packages": ["suite"], "failing_tests": ["suite::broken"]}),
        "the base replayed only the failing test"
    );
    assert_eq!(check.candidate_log["timed_out"], true);
}

// Fault injection: a base replay cut by a shortened review deadline is
// inconclusive and not cached, so it cannot suppress a later retry with
// enough budget to complete. Completed comparable results remain reusable.
// [ORB-15350]
#[test]
#[cfg(unix)]
fn a_base_replay_cut_by_a_shortened_deadline_is_inconclusive_and_not_cached() {
    let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    // Candidate fails immediately with `suite::broken`.
    // Base sleeps 3s when candidate.txt is absent, then fails with `suite::broken`.
    fs::write(
        repo.join("check.sh"),
        "#!/bin/sh\n\
         selection=\"$ORBIT_VALIDATION_SELECTION\"\n\
         [ -n \"$selection\" ] || selection='{\"packages\":[\"suite\"]}'\n\
         printf '{\"schema_version\":1,\"selection\":%s,\"tests_run\":1}' \"$selection\" \
         > \"$ORBIT_VALIDATION_SUMMARY\"\n\
         if [ ! -f candidate.txt ]; then sleep 3; fi\n\
         echo 'test suite::broken ... FAILED'\n\
         exit 1\n",
    )
    .unwrap();
    fs::set_permissions(repo.join("check.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    git_ok(&repo, &["add", "check.sh"]);
    git_ok(&repo, &["commit", "-q", "-m", "base"]);
    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    fs::write(repo.join("candidate.txt"), "after\n").unwrap();
    git_ok(&repo, &["add", "candidate.txt"]);
    git_ok(&repo, &["commit", "-q", "-m", "candidate"]);

    // (a) First attempt with a 2s budget: candidate fails in ~0.02s,
    // so base replay deadline is ~2s. Base sleeps 3s and times out.
    // The cut base run is inconclusive and must not be cached.
    let short_budget = ReplayBudget::new(Duration::from_secs(2));
    let check = verify_base_failure(
        &Host,
        &repo,
        base.trim(),
        "./check.sh",
        &["suite::broken".to_string()],
        "jrun-fixture-1",
        short_budget,
    )
    .unwrap();
    assert!(
        matches!(check.verdict, BaseFailureVerdict::Inconclusive(_)),
        "the deadline-cut base replay was inconclusive: {:?}",
        check.verdict
    );
    assert!(
        check.base_log["inconclusive"]
            .as_str()
            .unwrap()
            .contains("timed out before the comparison deadline"),
        "the base log records the cut timeout: {:?}",
        check.base_log
    );
    assert!(
        cached_base_check(&repo, base.trim(), "./check.sh", None).is_none(),
        "no conclusive base result was cached"
    );

    // (b) Retry with an 8s budget: candidate fails immediately, base has enough
    // time to finish its 3s sleep and completes with the same failure.
    // The failing test fails on the base too, so it is known red
    // [ORB-15546].
    let long_budget = ReplayBudget::new(Duration::from_secs(8));
    let check_retry = verify_base_failure(
        &Host,
        &repo,
        base.trim(),
        "./check.sh",
        &["suite::broken".to_string()],
        "jrun-fixture-2",
        long_budget,
    )
    .unwrap();
    assert_eq!(
        check_retry.verdict,
        BaseFailureVerdict::KnownRed {
            failures: vec!["test suite::broken".to_string()],
        },
        "with enough budget, the base failure is reproduced"
    );
    assert_eq!(check_retry.base_log["cached"], false);
    assert_eq!(check_retry.base_log["timed_out"], false);

    // (c) A subsequent call reuses the completed base result from the cache.
    let check_cached = verify_base_failure(
        &Host,
        &repo,
        base.trim(),
        "./check.sh",
        &["suite::broken".to_string()],
        "jrun-fixture-3",
        short_budget,
    )
    .unwrap();
    assert_eq!(
        check_cached.verdict,
        BaseFailureVerdict::KnownRed {
            failures: vec!["test suite::broken".to_string()],
        },
        "the cached base failure is reused"
    );
    assert_eq!(check_cached.base_log["cached"], true);
}

// Fault injection: reviewer comparisons preserve candidate-only identities,
// including against failed and unidentified bases and cached full results.
#[test]
#[cfg(unix)]
fn a_candidate_only_failure_yields_base_passes_and_completed_result_is_cached() {
    for (case, candidate_output, base_output, verdict) in [
        (
            "base-passes",
            "test suite::broken ... FAILED",
            None,
            "base_passes",
        ),
        (
            "shared-plus-new",
            "test suite::broken ... FAILED\ntest suite::added ... FAILED",
            Some("test suite::broken ... FAILED"),
            "candidate_adds",
        ),
        (
            "disjoint",
            "test suite::added ... FAILED",
            Some("test suite::broken ... FAILED"),
            "candidate_adds",
        ),
        (
            "noncomparable",
            "test suite::broken ... FAILED",
            Some("test suite::broken ... FAILED"),
            "not_comparable",
        ),
        (
            "base-unknown",
            "test suite::added ... FAILED",
            Some("unknown failure"),
            "inconclusive",
        ),
        (
            "both-unknown",
            "unknown failure",
            Some("unknown failure"),
            "inconclusive",
        ),
    ] {
        let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        // Preserve the original narrowed passing-base case. A mismatched
        // reported selection refuses even identical identities; the other
        // rows exercise full-selection comparisons and their cache.
        let summary = if case == "base-passes" {
            "selection=\"$ORBIT_VALIDATION_SELECTION\"; [ -n \"$selection\" ] || selection='{\"packages\":[\"suite\"]}'; printf '{\"schema_version\":1,\"selection\":%s,\"tests_run\":1}' \"$selection\" > \"$ORBIT_VALIDATION_SUMMARY\"\n"
        } else if case == "noncomparable" {
            "if [ -f candidate.txt ]; then selection='{\"packages\":[\"suite\"]}'; else selection='{\"packages\":[\"other\"]}'; fi; printf '{\"schema_version\":1,\"selection\":%s,\"tests_run\":1}' \"$selection\" > \"$ORBIT_VALIDATION_SUMMARY\"\n"
        } else {
            ""
        };
        let base_script = base_output.map_or_else(
            || "exit 0".to_string(),
            |output| format!("printf '%s\\n' '{output}'; exit 1"),
        );
        fs::write(repo.join("check.sh"), format!(
            "#!/bin/sh\n{summary}if [ -f candidate.txt ]; then printf '%s\\n' '{candidate_output}'; exit 1; fi\n{base_script}\n"
        )).unwrap();
        fs::set_permissions(repo.join("check.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        git_ok(&repo, &["add", "check.sh"]);
        git_ok(&repo, &["commit", "-q", "-m", "base"]);
        let base = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        fs::write(repo.join("candidate.txt"), "after\n").unwrap();
        git_ok(&repo, &["add", "candidate.txt"]);
        git_ok(&repo, &["commit", "-q", "-m", "candidate"]);
        for cached in [false, true] {
            let check = verify_base_failure(
                &Host,
                &repo,
                base.trim(),
                "./check.sh",
                &[],
                "jrun-fixture",
                ReplayBudget::new(Duration::from_secs(4)),
            )
            .unwrap();
            match verdict {
                "base_passes" => assert!(
                    matches!(check.verdict, BaseFailureVerdict::BasePasses(_)),
                    "{case}: {:?}",
                    check.verdict
                ),
                "candidate_adds" => assert_eq!(
                    check.verdict,
                    BaseFailureVerdict::CandidateAdds {
                        failures: vec!["test suite::added".to_string()]
                    },
                    "{case}"
                ),
                "inconclusive" => assert!(
                    matches!(&check.verdict, BaseFailureVerdict::Inconclusive(reason)
                    if reason.contains("failure identities are missing")),
                    "{case}: {:?}",
                    check.verdict
                ),
                "not_comparable" => assert!(
                    matches!(&check.verdict, BaseFailureVerdict::NotComparable(_)),
                    "{case}: {:?}",
                    check.verdict
                ),
                _ => unreachable!(),
            }
            assert_eq!(check.base_log["cached"], cached, "{case}");
            assert_eq!(
                check.base_replay,
                Some(if !summary.is_empty() {
                    BaseReplay::FailingTests
                } else if cached {
                    BaseReplay::CachedFailureSet
                } else {
                    BaseReplay::FullSelection
                }),
                "{case}"
            );
            if verdict == "candidate_adds" {
                assert!(
                    check.candidate_log["failures"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("test suite::added"))
                );
                assert_eq!(check.base_log["failures"], json!(["test suite::broken"]));
            }
        }
    }
}
