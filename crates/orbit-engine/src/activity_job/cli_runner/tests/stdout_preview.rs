#![allow(missing_docs)]

use orbit_common::security::redaction::argv_redactor;

use super::super::stdout_preview::stdout_text_preview;

#[test]
fn stdout_text_preview_redacts_a_secret_that_straddles_the_64kib_cut() {
    let secret = "orbit-preview-window-secret-value";
    let _guard =
        orbit_common::test_env::scoped([("ORBIT_PREVIEW_WINDOW_TEST_TOKEN", Some(secret))]);
    // Leave room for `[REDACTED_ENV]` inside the final 64 KiB prefix after
    // substitution. The secret itself still crosses the raw 64 KiB cut.
    let raw = format!(
        "{}{secret}{}",
        "x".repeat(64 * 1024 - 40),
        "y".repeat(8 * 1024)
    );
    let preview = stdout_text_preview(&raw, argv_redactor(), false);
    assert!(preview.truncated);
    assert!(preview.text.len() <= 64 * 1024);
    assert!(
        !preview.text.contains(secret),
        "secret straddling the 64 KiB cut must be redacted in the windowed preview"
    );
    assert!(preview.text.contains("[REDACTED_ENV]"));
}

#[test]
fn stdout_text_preview_redacts_a_secret_that_straddles_the_window_cap_after_shrinkage() {
    const LIMIT: usize = 64 * 1024;
    const MARGIN: usize = 1024;
    const CAP: usize = LIMIT + MARGIN;
    const RAW_LEN: usize = 100 * 1024;
    const SECRET_LEN: usize = 600;
    const COPIES_IN_TRUSTED: usize = 3;

    let mut secret = String::from("orbit-cap-straddle-");
    secret.push_str(&"s".repeat(SECRET_LEN - secret.len()));
    let _guard = orbit_common::test_env::scoped([(
        "ORBIT_PREVIEW_CAP_STRADDLE_TEST_TOKEN",
        Some(secret.as_str()),
    )]);

    for prefer_tail in [false, true] {
        let raw = if prefer_tail {
            let window_start = RAW_LEN - CAP;
            let straddle_start = window_start - SECRET_LEN / 2;
            let trusted_tail_start = RAW_LEN - LIMIT;
            let mut raw = String::with_capacity(RAW_LEN);
            raw.push_str(&"x".repeat(straddle_start));
            raw.push_str(&secret);
            raw.push_str(&"y".repeat(trusted_tail_start - raw.len()));
            for _ in 0..COPIES_IN_TRUSTED {
                raw.push_str(&secret);
            }
            raw.push_str(&"z".repeat(RAW_LEN - raw.len()));
            raw
        } else {
            let straddle_start = CAP - SECRET_LEN / 2;
            let mut raw = String::with_capacity(RAW_LEN);
            for _ in 0..COPIES_IN_TRUSTED {
                raw.push_str(&secret);
            }
            raw.push_str(&"x".repeat(straddle_start - raw.len()));
            raw.push_str(&secret);
            raw.push_str(&"y".repeat(RAW_LEN - raw.len()));
            raw
        };

        assert_eq!(raw.len(), RAW_LEN);
        let preview = stdout_text_preview(&raw, argv_redactor(), prefer_tail);
        assert!(
            preview.truncated,
            "100 KiB capture omits bytes (prefer_tail={prefer_tail})"
        );
        assert!(preview.text.len() <= LIMIT);
        // The cap-straddling copy is split, so only a half (~300 bytes) sits
        // in the window. A full-secret `contains` would miss that fragment.
        let leading = &secret[..SECRET_LEN / 2];
        let trailing = &secret[SECRET_LEN / 2..];
        assert!(
            !preview.text.contains(&secret)
                && !preview.text.contains(leading)
                && !preview.text.contains(trailing),
            "no byte of the cap-straddling secret may appear after shrinkage (prefer_tail={prefer_tail})"
        );
        assert!(
            preview.text.contains("[REDACTED_ENV]"),
            "trusted-region copies must still redact (prefer_tail={prefer_tail})"
        );
    }
}

#[test]
fn stdout_text_preview_truncated_is_false_when_redacted_capture_fits() {
    const LIMIT: usize = 64 * 1024;
    let mut secret = String::from("orbit-preview-fits-");
    secret.push_str(&"s".repeat(600 - secret.len()));
    let _guard =
        orbit_common::test_env::scoped([("ORBIT_PREVIEW_FITS_TEST_TOKEN", Some(secret.as_str()))]);
    // Between 64 KiB and 65 KiB. One 600-byte env value shrinks the redacted
    // text under the limit, and the whole capture fits in the window.
    let raw = format!("{}{}", secret, "x".repeat(LIMIT + 500 - secret.len()));
    assert!(raw.len() > LIMIT);
    assert!(raw.len() <= LIMIT + 1024);

    for prefer_tail in [false, true] {
        let preview = stdout_text_preview(&raw, argv_redactor(), prefer_tail);
        assert!(
            !preview.truncated,
            "complete in-window redacted capture must not be marked truncated (prefer_tail={prefer_tail})"
        );
        assert!(!preview.text.contains(&secret));
        assert!(preview.text.contains("[REDACTED_ENV]"));
        assert_eq!(
            preview.text.len(),
            raw.len() - secret.len() + "[REDACTED_ENV]".len()
        );
    }
}

#[test]
fn stdout_text_preview_truncated_is_true_when_in_window_text_still_exceeds_limit() {
    const LIMIT: usize = 64 * 1024;
    let raw = "x".repeat(LIMIT + 500);
    for prefer_tail in [false, true] {
        let preview = stdout_text_preview(&raw, argv_redactor(), prefer_tail);
        assert!(
            preview.truncated,
            "clipping a 64–65 KiB capture that does not shrink omits bytes (prefer_tail={prefer_tail})"
        );
        assert!(preview.text.len() <= LIMIT);
        assert_ne!(preview.text, raw);
    }
}

#[test]
fn stdout_text_preview_does_not_redact_a_secret_only_past_the_window() {
    let secret = "orbit-preview-far-tail-secret-value";
    let _guard = orbit_common::test_env::scoped([("ORBIT_PREVIEW_TAIL_TEST_TOKEN", Some(secret))]);
    let raw = format!("{}{secret}", "x".repeat(80 * 1024));
    let preview = stdout_text_preview(&raw, argv_redactor(), false);
    assert!(preview.truncated);
    assert!(!preview.text.contains(secret));
    assert!(
        !preview.text.contains("[REDACTED_ENV]"),
        "redaction must not run over the discarded tail of a prefix preview"
    );
}
