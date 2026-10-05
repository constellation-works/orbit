//! Precedence and reporting for the shared filesystem rule evaluator.

use crate::policy::CompiledFsRules;

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
