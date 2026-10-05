#![allow(missing_docs)]

use orbit_common::security::redaction::argv_redactor;

use super::super::stdout_preview::{bounded_redacted_text, stdout_text_preview};

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
fn tail_preview_hides_boundary_secret_with_mixed_redaction_lengths() {
    const LIMIT: usize = 64 * 1024;
    const CAP: usize = LIMIT + 1024;
    const RAW_LEN: usize = 100 * 1024;
    const FRAGMENT: &str = "TAIL_SECRET_FRAGMENT";
    let short = "Q7v9";
    let shrinking = format!("later-secret-{}", "s".repeat(587));

    // The first case places the fragment just beyond the raw margin. Expanding
    // the short value makes a fixed redacted-byte cut stop before that margin.
    // The second also guards secrets whose in-window suffix exceeds the margin.
    for suffix_len in [1035, 3000] {
        let mut secret = "l".repeat(5000 - suffix_len);
        secret.push_str(&"m".repeat(100));
        secret.push_str(short);
        secret.push_str(&"m".repeat(suffix_len - 104 - FRAGMENT.len()));
        secret.push_str(FRAGMENT);
        let _guard = orbit_common::test_env::scoped([
            ("ORBIT_PREVIEW_BOUNDARY_TEST_TOKEN", Some(secret.as_str())),
            ("ORBIT_PREVIEW_EXPANDING_TEST_TOKEN", Some(short)),
            (
                "ORBIT_PREVIEW_SHRINKING_TEST_TOKEN",
                Some(shrinking.as_str()),
            ),
        ]);
        let window_start = RAW_LEN - CAP;
        let mut raw = "x".repeat(window_start - (secret.len() - suffix_len));
        raw.push_str(&secret);
        raw.push_str(&"y".repeat((RAW_LEN - LIMIT).saturating_sub(raw.len())));
        for _ in 0..3 {
            raw.push_str(&shrinking);
        }
        raw.push_str(&"z".repeat(RAW_LEN - raw.len() - "visible tail".len()));
        raw.push_str("visible tail");
        assert_eq!(raw.len(), RAW_LEN);

        let preview = stdout_text_preview(&raw, argv_redactor(), true);
        assert!(
            !preview.text.contains(FRAGMENT) && !preview.text.contains(&"m".repeat(32)),
            "ORB-13939: boundary-secret bytes must not survive mixed expansion/shrinkage (suffix_len={suffix_len})"
        );
        assert!(!preview.text.contains(short));
        assert!(!preview.text.contains(&shrinking));
        assert!(preview.text.contains("[REDACTED_ENV]"));
        assert!(preview.text.ends_with("visible tail"));
        assert!(preview.truncated);
        assert_eq!(preview.preview_bytes, preview.text.len());
        assert!(preview.preview_bytes <= LIMIT);
    }
}

#[test]
fn bounded_preview_preserves_selection_and_byte_metadata() {
    let secret = "captured-value-".repeat(400);
    let _guard = orbit_common::test_env::scoped([(
        "ORBIT_PREVIEW_COMPLETE_TEST_TOKEN",
        Some(secret.as_str()),
    )]);
    let compressed = format!("head\n{secret}\ntail\n");
    let fully_redacted = "head\n[REDACTED_ENV]\ntail\n";
    for (raw, limit, head, tail, truncated) in [
        ("head\nmiddle\ntail\n", 10, "head\nmiddl", "tail\n", true),
        ("αβγδε", 5, "αβ", "δε", true),
        ("complete\n", 9, "complete\n", "complete\n", false),
        ("", 0, "", "", false),
        ("omitted", 0, "", "", true),
        (
            compressed.as_str(),
            64,
            fully_redacted,
            fully_redacted,
            false,
        ),
    ] {
        for (prefer_tail, expected) in [(false, head), (true, tail)] {
            let preview = bounded_redacted_text(raw, argv_redactor(), prefer_tail, limit);
            assert_eq!(preview.text, expected);
            assert_eq!(preview.truncated, truncated);
            assert_eq!(preview.preview_bytes, expected.len());
            assert!(preview.preview_bytes <= limit);
        }
    }
}
