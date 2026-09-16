#![allow(missing_docs)]

use crate::template::TemplateContext;

use super::super::condition::*;

#[test]
fn test_simple_eq() {
    assert!(evaluate_expr("success == success").unwrap());
    assert!(!evaluate_expr("success == failed").unwrap());
}

#[test]
fn test_simple_neq() {
    assert!(evaluate_expr("success != failed").unwrap());
    assert!(!evaluate_expr("success != success").unwrap());
}

#[test]
fn test_neq_numeric_string_zero() {
    assert!(!evaluate_expr("0 != 0").unwrap());
    assert!(evaluate_expr("3 != 0").unwrap());
}

#[test]
fn test_rendered_bundle_count_skip_guard() {
    let mut ctx = TemplateContext::default();
    std::sync::Arc::make_mut(&mut ctx.steps).insert(
        "validate_bundles".to_string(),
        serde_json::json!({
            "output": {
                "bundle_count": 0
            }
        }),
    );

    let result = evaluate_bool_expr(
        "{{ steps.validate_bundles.output.bundle_count }} != 0",
        &ctx,
    )
    .unwrap();
    assert!(!result);
}

#[test]
fn test_boolean_literals() {
    let ctx = TemplateContext::default();

    assert!(evaluate_bool_expr("true", &ctx).unwrap());
    assert!(!evaluate_bool_expr("false", &ctx).unwrap());
    assert!(evaluate_expr("false || true && true").unwrap());
    assert!(!evaluate_expr("true && false || false").unwrap());
}

#[test]
fn test_and() {
    assert!(evaluate_expr("a == a && b == b").unwrap());
    assert!(!evaluate_expr("a == a && b == c").unwrap());
}

#[test]
fn test_or() {
    assert!(evaluate_expr("a == b || c == c").unwrap());
    assert!(!evaluate_expr("a == b || c == d").unwrap());
}

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
fn test_whitespace_handling() {
    assert!(evaluate_expr("  success  ==  success  ").unwrap());
    assert!(evaluate_expr("a == a  &&  b == b").unwrap());
}

#[test]
fn test_invalid_atom() {
    assert!(evaluate_expr("no_operator_here").is_err());
}
