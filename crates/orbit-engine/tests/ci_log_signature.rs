//! Public CI signature and bounded evidence regressions.

use orbit_engine::ci_log_signature::{
    LineKind, classify_log_lines, diagnostic_excerpt, error_signature,
};

#[test]
fn guard_timings_do_not_hide_the_supply_chain_diagnostic() {
    let diagnostic = "error[vulnerability]: Multiple timing variability issues";
    let log = format!(
        "ci-guardrails: check-error-translation.sh 5.785 s\n{}\n{diagnostic}\n\
         ci-guardrails-phase: supply-chain wall_seconds=4 exit=1\n\
         ##[error]Process completed with exit code 1.\n",
        "passing guard output\n".repeat(830),
    );
    let signature = error_signature(&log, "Run CI guardrails");
    assert_eq!(signature.text, diagnostic.to_ascii_lowercase());
    assert!(!signature.step_fallback);
    assert!(diagnostic_excerpt(&log, 1024).contains(diagnostic));

    // Success output after a real error must not become the nearest marker.
    for noise in [
        "ci-guardrails: check-error-translation.sh 5.785 s",
        "SCCACHE_IGNORE_SERVER_IO_ERROR: 1",
        "scripts/error/check.sh",
        r"C:\error\check.sh",
        "error.log",
        "--no-fail-fast",
    ] {
        assert!(
            classify_log_lines(noise)
                .iter()
                .all(|(kind, _)| *kind == LineKind::Content),
            "identifier/path/option is not emitted diagnostic evidence: {noise}"
        );
        let log =
            format!("error: actual failure\n{noise}\n##[error]Process completed with exit code 1.");
        assert_eq!(
            error_signature(&log, "guardrails").text,
            "error: actual failure"
        );
    }
}

#[test]
fn nearest_diagnostic_keeps_compiler_test_and_annotation_precedence() {
    let ranks = [
        "error[E0062]: duplicate field `owner`",
        "test failing_case ... FAILED",
        "##[error]specific runner diagnostic",
        "error[vulnerability]: timing variability",
        "error: actual failure",
    ];
    for (index, strongest) in ranks.iter().enumerate() {
        for weaker in &ranks[index + 1..] {
            let log =
                format!("{strongest}\n{weaker}\n##[error]Process completed with exit code 1.");
            assert_eq!(
                error_signature(&log, "CI").text,
                orbit_engine::ci_log_signature::normalize_signature(
                    &strongest.to_ascii_lowercase()
                )
            );
        }
    }
    for (first, last) in [
        ("error[E0062]: first", "error[E0063]: last"),
        ("test first_case ... FAILED", "test last_case ... FAILED"),
        (
            "thread 'first' panicked at tests/a.rs:1:1:",
            "thread 'last' panicked at tests/b.rs:1:1:",
        ),
        ("##[error]first annotation", "##[error]last annotation"),
        ("error[vulnerability]: first", "error[license]: last"),
        ("error: first", "fatal: last"),
    ] {
        let log = format!("{first}\n{last}\n##[error]Process completed with exit code 1.");
        assert_eq!(
            error_signature(&log, "CI").text,
            orbit_engine::ci_log_signature::normalize_signature(&last.to_ascii_lowercase())
        );
        let with_cleanup = format!("{log}\nerror[E0001]: cleanup failure");
        assert_eq!(
            error_signature(&with_cleanup, "CI").text,
            error_signature(&log, "CI").text,
            "post-exit cleanup must not replace the failing process's diagnostic"
        );
    }
}
