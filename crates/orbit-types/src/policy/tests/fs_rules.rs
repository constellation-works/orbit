//! Precedence and reporting for the shared filesystem rule evaluator.

use crate::policy::{CompiledFsRules, FsOperation, ResolvedFsProfile};

fn rules(rules: &[&str]) -> CompiledFsRules {
    CompiledFsRules::compile(
        &rules.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "test",
    )
    .expect("compile rules")
}

#[test]
fn a_later_exclusion_overrides_an_earlier_grant() {
    let compiled = rules(&["**", "!**/.env"]);

    assert!(compiled.allows("src/main.rs").expect("allow source"));
    assert!(!compiled.allows("src/.env").expect("deny env"));
}

#[test]
fn the_reported_rule_names_the_pattern_that_settled_the_path() {
    let compiled = rules(&["**", "!**/.env"]);

    assert_eq!(
        compiled
            .evaluate("src/.env")
            .expect("evaluate")
            .matched_rule,
        "**/.env"
    );
    assert_eq!(
        compiled
            .evaluate("src/main.rs")
            .expect("evaluate")
            .matched_rule,
        "**"
    );
}

#[test]
fn an_unmatched_path_is_denied_and_distinguished_from_an_empty_rule_set() {
    let with_grants = rules(&["docs/**"]);
    let only_exclusions = rules(&["!**/.env"]);
    let empty = rules(&[]);

    assert_eq!(
        with_grants
            .evaluate("src/main.rs")
            .expect("evaluate")
            .matched_rule,
        "<no matching rule>"
    );
    assert_eq!(
        only_exclusions
            .evaluate("src/main.rs")
            .expect("evaluate")
            .matched_rule,
        "[]"
    );
    assert_eq!(
        empty
            .evaluate("src/main.rs")
            .expect("evaluate")
            .matched_rule,
        "[]"
    );
}

#[test]
fn a_rule_set_without_a_grant_cannot_allow_anything() {
    assert!(rules(&[]).grants_nothing());
    assert!(rules(&["!**/.env"]).grants_nothing());
    assert!(!rules(&["**", "!**/.env"]).grants_nothing());
}

#[test]
fn exclusions_are_reported_so_a_caller_can_skip_a_hole_punching_walk() {
    assert!(rules(&["**", "!**/.env"]).has_exclusion());
    assert!(!rules(&["**"]).has_exclusion());
}

#[test]
fn both_spellings_of_a_workspace_relative_path_decide_the_same() {
    let compiled = rules(&["**", "!**/.env"]);

    assert_eq!(
        compiled.allows("./src/.env").expect("dot slash"),
        compiled.allows("src/.env").expect("bare"),
    );
    assert!(compiled.allows(".").expect("workspace root"));
}

#[test]
fn a_resolved_profile_compiles_the_rule_set_for_the_requested_operation() {
    let profile = ResolvedFsProfile {
        name: "docs_writer".to_string(),
        read: vec!["**".to_string()],
        modify: vec!["docs/**".to_string()],
    };

    let read = profile.compile(FsOperation::Read).expect("compile read");
    let modify = profile
        .compile(FsOperation::Modify)
        .expect("compile modify");

    assert!(read.allows("src/main.rs").expect("read source"));
    assert!(!modify.allows("src/main.rs").expect("modify source"));
    assert!(modify.allows("docs/guide.md").expect("modify docs"));
}
