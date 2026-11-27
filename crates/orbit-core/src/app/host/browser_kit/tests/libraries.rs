use super::super::libraries::library_outcome;

#[test]
fn fonts_only_no_install_path_names_the_font_gap_and_a_fix() {
    let outcome = library_outcome("unresolved", Vec::new(), Vec::new(), true);

    assert_eq!(outcome.method, "unresolved");
    assert!(
        outcome
            .unresolved
            .iter()
            .any(|entry| entry.to_lowercase().contains("font")),
        "unresolved must name the font gap: {outcome:?}"
    );
    let fix = outcome.fix.expect("a font gap must offer a fix");
    assert!(
        fix.to_lowercase().contains("font"),
        "fix must name fonts: {fix}"
    );
}

#[test]
fn resolved_libraries_without_a_font_gap_stay_resolved() {
    let outcome = library_outcome("unpacked", vec!["pkg".to_string()], Vec::new(), false);

    assert_eq!(outcome.method, "unpacked");
    assert!(outcome.unresolved.is_empty());
    assert!(outcome.fix.is_none());
}
