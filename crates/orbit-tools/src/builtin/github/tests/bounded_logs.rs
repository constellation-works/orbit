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
