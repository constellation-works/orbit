use super::super::delivery_marker::delivery_markers;

// Parser edge cases meet the combinatorial pure-logic unit-test criterion.
#[test]
fn delivery_markers_preserve_valid_inner_candidates_and_marker_grammar() {
    let cases: &[(&str, &[&str])] = &[
        ("fix: buf[i [ORB-5]", &["[ORB-5]"]),
        ("feat: ... [see ORB-1… [ORB-5]", &["[ORB-5]"]),
        ("fix: [[[[ORB-5]]]]", &["[ORB-5]"]),
        ("fix: [buf[i [ORB-5] tail]", &["[ORB-5]"]),
        ("fix: é[備考 [ORB-5]", &["[ORB-5]"]),
        (
            "[ORB-5] [broken [T20260430-31B] [ORB-5]\n[GITHUB-PR-902]",
            &["[ORB-5]", "[T20260430-31B]", "[GITHUB-PR-902]"],
        ),
        (
            "[] [123] [letters] [ORB 5] [ORB\n5] [ORB\u{a0}5] [ORB-5",
            &[],
        ),
        ("ORB-5] plain text [unfinished", &[]),
        ("", &[]),
    ];
    for (message, expected) in cases {
        assert_eq!(delivery_markers(message), *expected, "message: {message:?}");
    }

    for (length, accepted) in [(64, true), (65, false)] {
        let token = format!("A{}5", "x".repeat(length - 2));
        let marker = format!("[{token}]");
        let message = format!("[unfinished {marker} [ORB-5]");
        let expected = if accepted {
            vec![marker.as_str(), "[ORB-5]"]
        } else {
            vec!["[ORB-5]"]
        };
        assert_eq!(
            delivery_markers(&message),
            expected,
            "token length: {length}"
        );
    }
}
