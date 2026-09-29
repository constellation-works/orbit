use super::{ceil_char_boundary, contains_lowercased, floor_char_boundary};

#[test]
fn boundaries_never_split_a_character() {
    let text = "a€b"; // '€' spans bytes 1..4.

    assert_eq!(floor_char_boundary(text, 2), 1);
    assert_eq!(ceil_char_boundary(text, 2), 4);
    assert_eq!(floor_char_boundary(text, 4), 4);
    assert_eq!(ceil_char_boundary(text, 1), 1);
    assert_eq!(floor_char_boundary(text, 99), text.len());
    assert_eq!(ceil_char_boundary(text, 99), text.len());
}

#[test]
fn contains_lowercased_matches_ascii_case_insensitively() {
    assert!(contains_lowercased("Fix the Parser BUG", "parser bug"));
    assert!(contains_lowercased("abc", ""));
    assert!(contains_lowercased("", ""));
    assert!(!contains_lowercased("", "a"));
    assert!(!contains_lowercased("short", "shorter"));
    assert!(!contains_lowercased("Fix the parser", "lexer"));
}

#[test]
fn contains_lowercased_agrees_with_lowercase_contains_for_unicode() {
    let cases = [
        ("Caf\u{c9} menu", "caf\u{e9}"),
        ("STRASSE \u{c4}RGER", "\u{e4}rger"),
        // KELVIN SIGN lowercases to ASCII 'k', so a non-ASCII haystack can
        // match an ASCII needle.
        ("\u{212A}ilo", "kilo"),
        ("plain ascii", "caf\u{e9}"),
        ("\u{65e5}\u{672c}\u{8a9e} text", "\u{672c}\u{8a9e}"),
        ("\u{65e5}\u{672c}\u{8a9e}", "text"),
        ("\u{130}stanbul", "i\u{307}stanbul"),
    ];
    for (haystack, needle) in cases {
        let needle = needle.to_lowercase();
        assert_eq!(
            contains_lowercased(haystack, &needle),
            haystack.to_lowercase().contains(&needle),
            "haystack {haystack:?} needle {needle:?}"
        );
    }
    assert!(contains_lowercased("\u{212A}ilo", "kilo"));
}
