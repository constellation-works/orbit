//! Bounding and redaction of runner-log excerpts, plus the checkout-evidence
//! scan that keeps the tested commit distinct from a run's reported head SHA.

use crate::builtin::github::{
    MAX_CHECKOUT_LOG_SCAN_BYTES, StreamedLogCollector, bound_log_text, scan_checkout_evidence,
};

#[test]
fn a_short_log_is_returned_whole_and_unmarked() {
    let bounded = bound_log_text("error: assertion failed\n", 1024);

    assert!(!bounded.truncated);
    assert_eq!(bounded.text, "error: assertion failed\n");
    assert_eq!(bounded.total_bytes, bounded.returned_bytes);
}

#[test]
fn an_oversized_log_is_capped_while_keeping_both_ends() {
    let raw = format!("HEAD-MARKER\n{}\nTAIL-MARKER", "x".repeat(200_000));

    let bounded = bound_log_text(&raw, 4_096);

    assert!(bounded.truncated);
    assert!(
        bounded.returned_bytes <= 4_096,
        "excerpt must respect the requested cap: {}",
        bounded.returned_bytes
    );
    assert_eq!(bounded.total_bytes, raw.len());
    assert!(bounded.text.contains("HEAD-MARKER"));
    assert!(bounded.text.contains("TAIL-MARKER"));
    assert!(bounded.text.contains("bytes omitted"));
}

#[test]
fn truncation_never_splits_a_multibyte_character() {
    let raw = "é".repeat(50_000);

    let bounded = bound_log_text(&raw, 1_001);

    assert!(bounded.truncated);
    // Reaching this point at all means both slices landed on char boundaries;
    // a mid-character split panics inside `bound_log_text`.
    assert!(bounded.text.contains('é'));
}

#[test]
fn a_credential_in_the_log_is_redacted_before_it_is_returned() {
    let raw = format!("remote: fatal: bad credentials ghp_{}\n", "a".repeat(36));

    let bounded = bound_log_text(&raw, 4_096);

    assert!(
        !bounded.text.contains("ghp_"),
        "token survived redaction: {}",
        bounded.text
    );
    assert!(bounded.text.contains("[REDACTED_SECRET]"));
}

#[test]
fn checkout_evidence_reports_the_tested_commit_not_the_event_sha() {
    let log = "\
setup\tRun actions/checkout@v4\t2026-01-01T00:00:00.0000000Z Syncing repository
setup\tRun actions/checkout@v4\t2026-01-01T00:00:01.0000000Z /usr/bin/git checkout --progress --force 1111111111111111111111111111111111111111
setup\tRun actions/checkout@v4\t2026-01-01T00:00:02.0000000Z HEAD is now at 1111111 chore: something
build\tRun tests\t2026-01-01T00:00:03.0000000Z error: build failed
";

    let evidence = scan_checkout_evidence(log, 40);

    assert!(
        evidence
            .commits
            .contains(&"1111111111111111111111111111111111111111".to_string()),
        "expected the full checked-out SHA: {:?}",
        evidence.commits
    );
    assert_eq!(evidence.lines.len(), 2);
    assert!(
        evidence
            .lines
            .iter()
            .any(|line| line.contains("HEAD is now at"))
    );
    assert!(
        evidence.lines.iter().all(|line| !line.contains('\t')),
        "the job and step columns are stripped: {:?}",
        evidence.lines
    );
}

/// The failure this scan is built to avoid. Every line of a checkout step is
/// labelled with the *action's* pinned SHA, and reporting that as the commit
/// under test would be evidence pointing at the wrong repository entirely.
#[test]
fn a_pinned_action_sha_is_never_reported_as_the_checked_out_commit() {
    let pin = "34e114876b0b11c390a56381ad16ebd13914f8d5";
    let tested = "5dbb8eff6f1ec88a24da618df1962d2c6b82ab6e";
    let log = format!(
        "\
macOS\tRun actions/checkout@{pin}\t2026-01-01T00:00:00.0000000Z ##[group]Checking out the ref
macOS\tRun actions/checkout@{pin}\t2026-01-01T00:00:01.0000000Z [command]/usr/bin/git checkout --progress --force -B trunk refs/remotes/origin/trunk
macOS\tRun actions/checkout@{pin}\t2026-01-01T00:00:02.0000000Z {tested}
"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert!(
        !evidence.commits.contains(&pin.to_string()),
        "the action pin leaked into the checkout evidence: {:?}",
        evidence.commits
    );
    assert_eq!(evidence.commits, vec![tested.to_string()]);
}

/// A bare SHA is evidence only inside a checkout step. The same shape shows up
/// in ordinary build output, where it means nothing.
#[test]
fn a_bare_sha_outside_a_checkout_step_is_not_treated_as_evidence() {
    let log = "\
build\tRun tests\t2026-01-01T00:00:00.0000000Z 5dbb8eff6f1ec88a24da618df1962d2c6b82ab6e
";

    let evidence = scan_checkout_evidence(log, 40);

    assert!(evidence.commits.is_empty(), "{:?}", evidence.commits);
    assert!(evidence.lines.is_empty());
}

/// A macOS runner emitted the command under `UNKNOWN STEP`, so step names
/// alone could not distinguish the following SHA from ordinary test output.
#[test]
fn git_log_head_command_in_an_unknown_step_names_its_following_sha() {
    let tested = "9a611e053bb440451cdfb5468749327853b12ff9";
    let log = format!(
        "macOS Platform\tUNKNOWN STEP\t2026-09-06T06:07:50.4897836Z [command]/usr/bin/git log -1 --format=%H\n\
         macOS Platform\tUNKNOWN STEP\t2026-09-06T06:07:50.4927165Z {tested}\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(evidence.commits, [tested]);
    assert!(evidence.complete);
    assert!(
        evidence
            .lines
            .iter()
            .any(|line| line.contains("git log -1 --format=%H")),
        "the command provenance must be retained: {:?}",
        evidence.lines
    );
}

#[test]
fn only_the_immediate_output_of_the_recognized_command_is_checkout_evidence() {
    let unrelated = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let delayed = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let log = format!(
        "ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0000000Z [command]/usr/bin/git rev-parse HEAD\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0010000Z {unrelated}\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0020000Z [command]/usr/bin/git log -1 --format=%H\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0030000Z not a SHA\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0040000Z {delayed}\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert!(evidence.commits.is_empty(), "{:?}", evidence.commits);
    assert!(evidence.lines.is_empty(), "{:?}", evidence.lines);
}

#[test]
fn multiple_recognized_command_outputs_remain_ambiguous() {
    let first = "cccccccccccccccccccccccccccccccccccccccc";
    let second = "dddddddddddddddddddddddddddddddddddddddd";
    let log = format!(
        "ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0000000Z [command]/usr/bin/git log -1 --format=%H\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0010000Z {first}\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0020000Z [command]/usr/bin/git log -1 --format=%H\n\
         ci\tUNKNOWN STEP\t2026-09-06T06:07:50.0030000Z {second}\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(evidence.commits, [first, second]);
    assert!(evidence.complete);
}

#[test]
fn checkout_evidence_lines_are_capped_and_redacted() {
    let line = format!(
        "setup\tcheckout\t2026-01-01T00:00:00.0000000Z HEAD is now at 2222222 token ghp_{}\n",
        "b".repeat(36)
    );
    let log = line.repeat(100);

    let evidence = scan_checkout_evidence(&log, 5);

    assert_eq!(evidence.lines.len(), 5);
    assert!(evidence.lines.iter().all(|line| !line.contains("ghp_")));
}

#[test]
fn a_log_without_checkout_evidence_yields_nothing_rather_than_a_guess() {
    let evidence = scan_checkout_evidence("build\tRun tests\terror: build failed\n", 40);

    assert!(evidence.commits.is_empty());
    assert!(evidence.lines.is_empty());
}

/// The commit list is bounded like the lines are: a matrix log with tens of
/// thousands of fetch lines must not hand the caller an unbounded array, and
/// every commit appears once.
#[test]
fn checkout_evidence_caps_and_dedups_commits() {
    let mut log = String::new();
    for index in 0..500_u32 {
        let sha = format!("{index:040x}");
        // Each commit appears twice, as a fetch line and a checkout line.
        log.push_str(&format!(
            "setup\tRun actions/checkout@v4\t2026-01-01T00:00:00.0000000Z  * branch {sha} -> FETCH_HEAD\n"
        ));
        log.push_str(&format!(
            "setup\tRun actions/checkout@v4\t2026-01-01T00:00:01.0000000Z HEAD is now at {sha} chore\n"
        ));
    }

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(evidence.commits.len(), 40, "{:?}", evidence.commits);
    assert_eq!(evidence.lines.len(), 40);
    let mut unique = evidence.commits.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), evidence.commits.len());
    assert_eq!(evidence.commits[0], format!("{:040x}", 0));
    // The display cap reduces what is reported, but every commit up to the
    // cap was still seen — this is not the same as missing identity.
    assert!(
        evidence.complete,
        "a display-count cap alone must not mark identity incomplete"
    );
    assert!(evidence.display_truncated);
}

/// An overlong line that has nothing to do with checkout (ordinary build
/// spam) is dropped for display reasons only: it could never have carried
/// checkout identity, so losing it must not taint the identity verdict.
#[test]
fn an_overlong_line_unrelated_to_checkout_is_dropped_without_marking_identity_incomplete() {
    let sha = "5".repeat(40);
    let overlong = "x".repeat(20_000);
    let log = format!(
        "build\tRun tests\t2026-01-01T00:00:00.0000000Z {overlong}\n\
         ci\tCheckout\t2026-01-01T00:00:01.0000000Z HEAD is now at {sha}\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert!(
        evidence.complete,
        "an unrelated overlong line must not mark identity incomplete"
    );
    assert!(evidence.display_truncated);
    assert_eq!(evidence.commits, vec![sha]);
}

/// An overlong line inside a checkout step could have carried the checked-out
/// commit; dropping it must leave identity marked incomplete rather than
/// silently reporting whatever else was found.
#[test]
fn an_overlong_checkout_line_is_dropped_and_marks_identity_incomplete() {
    let overlong = "z".repeat(20_000);
    let log = format!("ci\tCheckout\t2026-01-01T00:00:00.0000000Z {overlong}\n");

    let evidence = scan_checkout_evidence(&log, 40);

    assert!(
        !evidence.complete,
        "a dropped checkout-step line must mark identity incomplete"
    );
    assert!(evidence.commits.is_empty());
    assert!(
        !evidence.display_truncated,
        "identity incompleteness is distinct from a display-only cap"
    );
}

#[test]
fn an_overlong_line_consumes_pending_checkout_command_context() {
    let tested = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let overlong = "x".repeat(20_000);
    let log = format!(
        "ci\tUNKNOWN STEP\t[command]/usr/bin/git log -1 --format=%H\n\
         ci\tUNKNOWN STEP\t{overlong}\n\
         ci\tUNKNOWN STEP\t{tested}\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert!(evidence.commits.is_empty(), "{:?}", evidence.commits);
    assert!(evidence.complete);
    assert!(evidence.display_truncated);
}

#[test]
fn streaming_log_finds_middle_checkout_without_retaining_it_in_the_excerpt() {
    let sha = "2d773a649844b168c5cfce4c80feadb8b025bb69";
    let mut collector = StreamedLogCollector::new(96, 40);
    collector.push(b"head-marker\n");
    collector.push(&vec![b'x'; 400]);
    collector.push(format!("\nsetup\tCheckout\tHEAD is now at {sha}\n").as_bytes());
    collector.push(&vec![b'y'; 400]);
    collector.push(b"\ntail-marker\n");
    let log = collector.finish();

    assert!(log.truncated);
    assert!(log.text.contains("head-marker"));
    assert!(log.text.contains("tail-marker"));
    assert!(
        !log.text.contains(sha),
        "checkout belongs in evidence, not excerpt"
    );
    assert_eq!(log.checkout_evidence.commits, [sha]);
    assert!(log.checkout_evidence.complete);
}

#[test]
fn streaming_log_marks_identity_incomplete_after_the_hard_scan_limit() {
    let sha = "2d773a649844b168c5cfce4c80feadb8b025bb69";
    let mut collector = StreamedLogCollector::new(64, 40);
    collector.push(&vec![b'x'; MAX_CHECKOUT_LOG_SCAN_BYTES]);
    collector.push(format!("\nsetup\tCheckout\tHEAD is now at {sha}\n").as_bytes());
    let log = collector.finish();

    assert!(log.checkout_evidence.source_truncated);
    assert!(!log.checkout_evidence.complete);
    assert!(log.checkout_evidence.commits.is_empty());
}

#[test]
fn command_output_past_the_hard_scan_limit_is_not_verified() {
    let tested = "2d773a649844b168c5cfce4c80feadb8b025bb69";
    let command = b"ci\tUNKNOWN STEP\t[command]/usr/bin/git log -1 --format=%H\n";
    let fill = MAX_CHECKOUT_LOG_SCAN_BYTES - command.len();
    let mut collector = StreamedLogCollector::new(64, 40);
    collector.push(command);
    collector.push(&vec![b'x'; fill]);
    collector.push(format!("\nci\tUNKNOWN STEP\t{tested}\n").as_bytes());
    let log = collector.finish();

    assert!(log.checkout_evidence.source_truncated);
    assert!(!log.checkout_evidence.complete);
    assert!(log.checkout_evidence.commits.is_empty());
}

/// jrun-20260905-1932, run 33986085270: one commit printed in full on the
/// checkout command line and abbreviated on the `HEAD is now at` line. Two
/// spellings of one commit are not a contradiction, and reporting them as two
/// made an otherwise complete investigation look ambiguous.
#[test]
fn an_abbreviated_and_a_full_spelling_are_one_commit() {
    let tested = "4968f137ab9969c35634496414fdc1637d22fe37";
    let log = format!(
        "macOS\tRun actions/checkout@v5\t2026-09-05T19:20:00.0000000Z [command]/usr/bin/git checkout --progress --force {tested}\n\
         macOS\tRun actions/checkout@v5\t2026-09-05T19:20:01.0000000Z HEAD is now at 4968f13 feat: add Pi\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(evidence.commits, vec![tested.to_string()]);
    assert!(evidence.complete);
}

/// jrun-20260905-1932, run 33986582084: a pull-request merge checkout whose
/// subject quotes both parents. The merge commit is what the runner tested;
/// the parents are prose that happens to be hex.
#[test]
fn merge_parents_quoted_in_a_checkout_subject_are_not_checkout_identity() {
    let merge = "7dcd45b8a214e861ad533c6d03257cd3948514f4";
    let head = "d5b3f2a1c7e94806b1ad3ee0f1c2a9b8d7e6f504";
    let base = "22037486aa1bb2cc3dd4ee5ff60718293a4b5c6d";
    let log = format!(
        "website\tRun actions/checkout@v5\t2026-09-05T19:20:00.0000000Z  * branch {head} -> FETCH_HEAD\n\
         website\tRun actions/checkout@v5\t2026-09-05T19:20:01.0000000Z HEAD is now at {merge} Merge {head} into {base}\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(evidence.commits, vec![merge.to_string()]);
    assert!(
        evidence.lines.iter().any(|line| line.contains(head)),
        "the fetched head stays visible as evidence: {:?}",
        evidence.lines
    );
}

/// The reductions above must not become a way to launder a real disagreement
/// into a confident answer.
#[test]
fn two_checkout_steps_naming_different_commits_stay_two_commits() {
    let first = "3333333333333333333333333333333333333333";
    let second = "4444444444444444444444444444444444444444";
    let log = format!(
        "ci\tCheckout\t2026-09-05T19:20:00.0000000Z HEAD is now at {first} first\n\
         ci\tCheckout\t2026-09-05T19:20:01.0000000Z HEAD is now at {second} second\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(
        evidence.commits,
        vec![first.to_string(), second.to_string()]
    );
}

/// An abbreviation with two candidate expansions is left abbreviated: picking
/// one would be a guess presented as evidence.
#[test]
fn an_abbreviation_with_rival_expansions_is_not_resolved_to_a_guess() {
    let short = "abc1234";
    let first = "abc1234000000000000000000000000000000000";
    let second = "abc1234111111111111111111111111111111111";
    let log = format!(
        "ci\tCheckout\t2026-09-05T19:20:00.0000000Z  * branch {first} -> FETCH_HEAD\n\
         ci\tCheckout\t2026-09-05T19:20:01.0000000Z  * branch {second} -> FETCH_HEAD\n\
         ci\tCheckout\t2026-09-05T19:20:02.0000000Z HEAD is now at {short} ambiguous\n"
    );

    let evidence = scan_checkout_evidence(&log, 40);

    assert_eq!(evidence.commits, vec![short.to_string()]);
}

fn command_unit() -> String {
    "2026-09-07T20:52:05Z ##[group]Run cargo check\n\
     2026-09-07T20:52:05Z shell: /usr/bin/bash -e {0}\n\
     2026-09-07T20:52:05Z ##[endgroup]\n\
     2026-09-07T20:52:23Z error[E0063]: missing field `owner_machine_id`\n\
     2026-09-07T20:52:23Z   --> crates/orbit-core/src/runtime.rs:92:7\n\
     2026-09-07T20:52:23Z    | WorkspaceRuntimeBinding {\n\
     2026-09-07T20:52:23Z    | ^ missing `owner_machine_id`\n\
     2026-09-07T20:52:31Z ##[error]Process completed with exit code 101.\n"
        .to_string()
}

#[test]
fn long_log_retains_complete_middle_command_independently_of_display() {
    let unit = command_unit();
    let raw = format!(
        "{}{}{}",
        "setup output\n".repeat(3000),
        unit,
        "cleanup output\n".repeat(3000)
    );
    for chunk_size in [1, 7, 4096] {
        let mut collector = StreamedLogCollector::new(16_384, 40);
        for chunk in raw.as_bytes().chunks(chunk_size) {
            collector.push(chunk);
        }
        let log = collector.finish();
        assert!(log.truncated);
        assert!(!log.text.contains("owner_machine_id"));
        assert_eq!(log.diagnostic.as_deref(), Some(unit.as_str()));
    }
}

#[test]
fn incomplete_ambiguous_and_source_limited_commands_are_not_complete_units() {
    let unit = command_unit();
    for raw in [
        unit.replace("##[group]Run cargo check", "cargo check"),
        unit.replace("##[error]Process completed with exit code 101.\n", ""),
        format!("{unit}{unit}"),
        format!("{unit}##[warning]Log output was truncated\n"),
        format!("{unit}partial trailing line"),
        format!("{}\n{unit}", "x".repeat(MAX_CHECKOUT_LOG_SCAN_BYTES)),
        unit.replace(
            "shell: /usr/bin/bash -e {0}",
            &"build output\n".repeat(30_000),
        ),
    ] {
        let mut collector = StreamedLogCollector::new(16_384, 40);
        collector.push(raw.as_bytes());
        assert!(collector.finish().diagnostic.is_none());
    }
}

#[test]
fn selected_command_preserves_unicode_and_redacts_secrets_across_chunk_boundaries() {
    let raw = command_unit().replace(
        "missing field",
        &format!("missing café ghp_{} field", "a".repeat(36)),
    );
    let mut collector = StreamedLogCollector::new(64, 40);
    for byte in raw.as_bytes() {
        collector.push(std::slice::from_ref(byte));
    }
    let unit = collector
        .finish()
        .diagnostic
        .expect("complete redacted unit");
    assert!(unit.contains("café"));
    assert!(unit.contains("[REDACTED_SECRET]"));
    assert!(!unit.contains("ghp_"));
}

fn oversized_test_command() -> String {
    format!(
        "##[group]Run cargo nextest run\n##[endgroup]\n{}\n\
         FAIL [ 0.1s] suite first_failure\n\
         thread 'first_failure' panicked at tests/golden.rs:12:5:\n\
         assertion `left == right` failed: tool_list.plain.txt golden drift\n\
           left: {}\n  right: expected golden\n\
         {}\n\
         FAIL [ 0.2s] suite second_failure\n\
         thread 'second_failure' panicked at tests/other.rs:20:7:\n\
         assertion failed: second condition\n\
         Summary [ 1.0s] 2 tests run: 2 failed\n\
         ##[error]Process completed with exit code 100.\n",
        "PASS ordinary_test\n".repeat(20_000),
        "café ".repeat(20_000),
        "PASS another_test\n".repeat(100),
    )
}

#[test]
fn oversized_command_keeps_all_failure_regions_and_counts_assertion_omissions() {
    let raw = oversized_test_command();
    for chunk_size in [1, 7, 4096] {
        let mut collector = StreamedLogCollector::new(128, 40);
        for chunk in raw.as_bytes().chunks(chunk_size) {
            collector.push(chunk);
        }
        let log = collector.finish();
        assert!(log.source_complete);
        assert!(log.diagnostic.is_none());
        let regions = log.failure_regions.expect("bounded failure regions");
        assert_eq!(regions["complete"], false);
        assert_eq!(regions["command_complete"], true);
        assert_eq!(regions["selection_complete"], true);
        assert_eq!(regions["command_bytes"], raw.len());
        assert_eq!(
            regions["retained_source_bytes"].as_u64().expect("retained")
                + regions["omitted_bytes"].as_u64().expect("omitted"),
            raw.len() as u64
        );
        assert!(
            regions["assertion_payload_omitted_bytes"]
                .as_u64()
                .expect("assertions")
                > 100_000
        );
        let text = regions["text"].as_str().expect("text");
        for expected in [
            "first_failure",
            "second_failure",
            "golden.rs:12:5",
            "other.rs:20:7",
            "golden drift",
            "right: expected",
            "Summary",
            "exit code 100",
            "assertion payload bytes omitted",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(text.len() < 4_000);
    }
}

#[test]
fn partial_regions_never_override_missing_source_ambiguous_commands_or_hidden_columns() {
    let raw = oversized_test_command();
    for invalid in [
        format!("{raw}{raw}"),
        format!("{raw}##[warning]Log output was truncated\n"),
        raw.replace("##[error]Process completed with exit code 100.\n", ""),
        format!("{raw}partial line"),
        raw.replace("PASS another_test", "error: too many distinct failures")
            .repeat(2),
        // A conflicting column in discarded chatter must still invalidate
        // attribution, even though the retained failure lines all match.
        raw.lines()
            .enumerate()
            .map(|(index, line)| {
                format!(
                    "{}\tTests\t{line}\n",
                    if index == 500 { "Other" } else { "CI" }
                )
            })
            .collect(),
    ] {
        let mut collector = StreamedLogCollector::new(128, 40);
        collector.push(invalid.as_bytes());
        let log = collector.finish();
        assert!(log.diagnostic.is_none());
        assert!(log.failure_regions.is_none());
    }
}

#[test]
fn failure_region_overflow_defers_instead_of_losing_secondary_failures() {
    let raw = format!(
        "##[group]Run tests\n{}##[error]Process completed with exit code 1.\n",
        "thread 'another_failure' panicked at tests/example.rs:1:1:\n".repeat(10_000)
    );
    let mut collector = StreamedLogCollector::new(128, 40);
    collector.push(raw.as_bytes());
    let log = collector.finish();
    assert!(log.source_complete);
    assert!(log.diagnostic.is_none());
    assert!(log.failure_regions.is_none());
}

#[test]
fn assertion_prefix_never_leaks_a_secret_cut_at_the_retention_boundary() {
    let raw = oversized_test_command().replace(
        &"café ".repeat(20_000),
        &format!(
            "{}ghp_{} {}",
            "x ".repeat(245),
            "a".repeat(36),
            "tail ".repeat(20_000)
        ),
    );
    let mut collector = StreamedLogCollector::new(128, 40);
    collector.push(raw.as_bytes());
    let log = collector.finish();
    let regions = log.failure_regions.expect("regions");
    assert!(!regions["text"].as_str().expect("text").contains("ghp_"));
}
