#![allow(missing_docs)]

use crate::template::TemplateContext;

use super::super::condition::*;

#[test]
fn test_precedence_and_binds_tighter() {
    // "false || true && true" → false || (true && true) → true
    assert!(evaluate_expr("a == b || c == c && d == d").unwrap());
    // "true && false || true" → (true && false) || true → true
    assert!(evaluate_expr("a == a && b == c || d == d").unwrap());
    // "false && true || false" → (false && true) || false → false
    assert!(!evaluate_expr("a == b && c == c || d == e").unwrap());
}

#[test]
fn a_rendered_value_is_compared_not_parsed() {
    let mut ctx = TemplateContext::default();
    std::sync::Arc::make_mut(&mut ctx.steps).insert(
        "review".to_string(),
        serde_json::json!({ "output": { "verdict": "a == a || x" } }),
    );

    let approved =
        evaluate_bool_expr("{{ steps.review.output.verdict }} == approved", &ctx).unwrap();
    assert!(
        !approved,
        "operators inside a rendered value must not add branches"
    );

    let unchanged =
        evaluate_bool_expr("{{ steps.review.output.verdict }} != approved", &ctx).unwrap();
    assert!(unchanged);
}
