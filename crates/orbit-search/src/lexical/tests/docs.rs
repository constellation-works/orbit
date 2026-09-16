//! Unit tests for docs lexical scoring.

use super::super::docs::*;

fn doc(path: &str, summary: &str) -> DocSearchSource {
    DocSearchSource {
        path: path.to_string(),
        doc_type: "design".to_string(),
        summary: summary.to_string(),
        tags: vec!["orbit-docs".to_string()],
        paths: Vec::new(),
        related_features: Vec::new(),
        related_artifacts: Vec::new(),
        body: "A complete decision body".to_string(),
    }
}

#[test]
fn score_doc_record_matches_inlined_body_and_returns_snippet() {
    let mut record = doc("docs/design/example/4_decisions.md", "Decision log");
    record.body = "Before the heliotrope-dispatch choice, requests were queued.".to_string();

    let result = score_doc_record(record, "heliotrope-dispatch").expect("body match");

    assert_eq!(result.matched_by, vec!["body"]);
    assert!(
        result
            .snippet
            .is_some_and(|snippet| snippet.contains("heliotrope-dispatch"))
    );
}

#[test]
fn score_doc_record_matches_summary_type_and_tags() {
    let summary =
        score_doc_record(doc("docs/a.md", "Inline ADR entries"), "adr").expect("summary match");
    assert_eq!(summary.matched_by, vec!["summary"]);

    let tag = score_doc_record(doc("docs/a.md", "Decisions"), "orbit-docs").expect("tag match");
    assert_eq!(tag.matched_by, vec!["tag:orbit-docs"]);

    let kind = score_doc_record(doc("docs/a.md", "Decisions"), "design").expect("type match");
    assert_eq!(kind.matched_by, vec!["type:design"]);
}

#[test]
fn sort_search_results_breaks_ties_by_path() {
    let mut results = vec![
        SearchResult::Doc(score_doc_record(doc("docs/b.md", "ADR body"), "adr").expect("b")),
        SearchResult::Doc(score_doc_record(doc("docs/a.md", "ADR body"), "adr").expect("a")),
    ];
    sort_search_results(&mut results);
    let paths = results
        .iter()
        .map(|result| match result {
            SearchResult::Doc(result) => result.record.path.as_str(),
        })
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["docs/a.md", "docs/b.md"]);
}

/// [DANI-10369] The body match no longer lowercases the whole body first, so
/// the in-place scan must still be case-insensitive and must still hand back
/// an offset that slices `record.body` safely past multi-byte characters.
#[test]
fn score_doc_record_body_match_is_case_insensitive_in_place() {
    let mut record = doc("docs/a.md", "Decision log");
    record.body = "Ünïcode prelude — then the HELIOTROPE-Dispatch choice was made.".to_string();

    let result = score_doc_record(record, "heliotrope-dispatch").expect("body match");

    assert_eq!(result.matched_by, vec!["body"]);
    assert_eq!(
        result.snippet.as_deref(),
        Some("Ünïcode prelude — then the HELIOTROPE-Dispatch choice was made.")
    );
}

#[test]
fn find_ignore_ascii_case_matches_the_lowercased_find() {
    let haystack = "Ünïcode prelude — then the HELIOTROPE-Dispatch choice";
    assert_eq!(
        find_ignore_ascii_case(haystack, "heliotrope-dispatch"),
        haystack.to_ascii_lowercase().find("heliotrope-dispatch")
    );
    // Only ASCII folds, exactly as before: a non-ASCII letter must match as written.
    assert_eq!(find_ignore_ascii_case(haystack, "Ünïcode"), Some(0));
    assert_eq!(find_ignore_ascii_case(haystack, "ünïcode"), None);
    assert_eq!(find_ignore_ascii_case(haystack, "absent"), None);
    assert_eq!(find_ignore_ascii_case("", "x"), None);
}
