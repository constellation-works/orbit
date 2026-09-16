use std::collections::BTreeSet;

use crate::fs::overlap_index::OverlapIndex;
use crate::fs::selector::overlaps;

/// A corpus that exercises every rule `overlaps` has: shared anchors across
/// selector kinds, `dir:` and legacy containment in both directions, `file:`
/// non-containment, sibling anchors that share a textual prefix without a `/`
/// boundary, anchor-less selectors, absolute anchors, and unparseable input.
fn corpus() -> Vec<&'static str> {
    vec![
        "dir:src",
        "dir:src/",
        "src/",
        "src",
        "src-old",
        "file:src-old/lib.rs",
        "file:src/lib.rs",
        "symbol:src/lib.rs#run:function",
        "symbol:src/lib.rs#stop:function",
        "src/lib.rs:12",
        "dir:src/nested",
        "file:src/nested/deep/a.rs",
        "file:src/nested/deep/b.rs",
        "dir:crates",
        "file:crates/a/lib.rs",
        "file:lib/y.rs",
        "module:orbit::core",
        "module:orbit::core ",
        "module:orbit::web",
        "command:orbit task",
        "dir:/",
        "file:/etc/hosts",
        "dir:/etc",
        "dir:.",
        "file:./src/lib.rs",
        "",
        "   ",
        "symbol:broken",
    ]
}

fn brute_force(requested: &str, held: &[&str]) -> BTreeSet<String> {
    held.iter()
        .filter(|candidate| overlaps(requested, candidate))
        .map(|candidate| candidate.to_string())
        .collect()
}

fn indexed(index: &OverlapIndex<usize>, requested: &str) -> BTreeSet<String> {
    index
        .overlapping(requested)
        .into_iter()
        .map(|(selector, _)| selector.to_string())
        .collect()
}

#[test]
fn every_query_matches_the_pairwise_overlap_answer() {
    let held = corpus();
    let mut index = OverlapIndex::new();
    for (position, selector) in held.iter().enumerate() {
        let inserted = index.insert(selector, position);
        assert_eq!(
            inserted,
            crate::fs::selector::OverlapScope::parse(selector).is_some(),
            "insert reports whether `{selector}` parsed"
        );
    }

    for requested in corpus() {
        assert_eq!(
            indexed(&index, requested),
            brute_force(requested, &held),
            "overlap set for `{requested}`"
        );
    }
}

#[test]
fn values_ride_along_with_their_selector() {
    let mut index = OverlapIndex::new();
    index.insert("dir:src", vec!["ORB-1".to_string()]);
    index.insert(
        "file:src/lib.rs",
        vec!["ORB-2".to_string(), "ORB-3".to_string()],
    );
    index.insert("file:lib/y.rs", vec!["ORB-4".to_string()]);

    let mut hits = index.overlapping("symbol:src/lib.rs#run:function");
    hits.sort();
    assert_eq!(
        hits,
        vec![
            ("dir:src", &vec!["ORB-1".to_string()]),
            (
                "file:src/lib.rs",
                &vec!["ORB-2".to_string(), "ORB-3".to_string()]
            ),
        ]
    );
}

#[test]
fn a_file_query_does_not_reach_beneath_its_own_anchor() {
    let mut index = OverlapIndex::new();
    index.insert("file:src/lib.rs/odd", ());
    index.insert("dir:src/lib.rs/odd", ());

    assert!(index.overlapping("file:src/lib.rs").is_empty());
    assert_eq!(index.overlapping("dir:src/lib.rs").len(), 2);
}

#[test]
fn a_sibling_sharing_a_textual_prefix_is_not_a_descendant() {
    let mut index = OverlapIndex::new();
    index.insert("file:src-old/lib.rs", ());
    index.insert("file:src/lib.rs", ());
    index.insert("file:src0/lib.rs", ());

    let hits = index.overlapping("dir:src");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "file:src/lib.rs");
}

#[test]
fn unparseable_selectors_are_neither_indexed_nor_matched() {
    let mut index = OverlapIndex::new();
    assert!(!index.insert("", ()));
    assert!(!index.insert("symbol:broken", ()));
    assert!(index.is_empty());

    index.insert("dir:src", ());
    assert!(!index.is_empty());
    assert!(index.overlapping("").is_empty());
    assert!(index.overlapping("symbol:broken").is_empty());
}
