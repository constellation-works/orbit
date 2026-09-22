use super::super::namespace::{is_valid_namespace, is_valid_verb, namespace_collides_with_tool};

#[test]
fn underscore_is_refused_in_namespaces_and_verbs() {
    // `_` would let two plugins flatten to the same MCP name: `a_b` + `c` and
    // `a` + `b_c` both advertise as `a_b_c`.
    assert!(!is_valid_namespace("a_b"));
    assert!(!is_valid_verb("b_c"));
    assert!(is_valid_namespace("a-b"));
    assert!(is_valid_verb("b-c"));
}

#[test]
fn namespace_collides_with_tool_checks_the_owned_prefix_only() {
    assert!(namespace_collides_with_tool(
        "graph",
        false,
        "graph.recommend"
    ));
    assert!(namespace_collides_with_tool(
        "graph",
        true,
        "orbit.graph.recommend"
    ));
    assert!(!namespace_collides_with_tool(
        "graph",
        false,
        "graphite.recommend"
    ));
}
