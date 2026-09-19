use crate::task::{EPIC_TAG, EpicHierarchyNode, has_epic_tag, inherited_only_epic_roots};

fn epic_tags() -> Vec<String> {
    vec![EPIC_TAG.to_string()]
}

fn node<'a>(
    id: &'a str,
    parent_id: Option<&'a str>,
    tags: &'a [String],
    declares_context: bool,
) -> EpicHierarchyNode<'a> {
    EpicHierarchyNode {
        id,
        parent_id,
        tags,
        declares_context,
    }
}

#[test]
fn a_tagged_root_whose_child_declares_context_is_inherited_only() {
    let tags = epic_tags();
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("root", None, &tags, false),
        node("child", Some("root"), &none, true),
    ]);

    assert_eq!(roots.get("root"), Some(&vec!["child"]));
}

#[test]
fn descendants_are_reported_in_id_order_across_the_whole_subtree() {
    let tags = epic_tags();
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("root", None, &tags, false),
        node("b-child", Some("root"), &none, true),
        node("a-grandchild", Some("b-child"), &none, true),
        node("quiet-child", Some("root"), &none, false),
    ]);

    assert_eq!(roots.get("root"), Some(&vec!["a-grandchild", "b-child"]));
}

/// The rule is about a footprint that was actually inherited. A tagged family
/// that declares nothing anywhere had no union to lose, so it stays an ordinary
/// undeclared task rather than becoming unadmittable.
#[test]
fn a_family_that_declares_nothing_anywhere_is_not_withheld() {
    let tags = epic_tags();
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("root", None, &tags, false),
        node("child", Some("root"), &none, false),
    ]);

    assert!(roots.is_empty());
}

#[test]
fn a_tagged_root_that_declares_its_own_surface_is_an_ordinary_leaf() {
    let tags = epic_tags();
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("root", None, &tags, true),
        node("child", Some("root"), &none, true),
    ]);

    assert!(roots.is_empty());
}

#[test]
fn an_untagged_parent_is_never_a_root() {
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("parent", None, &none, false),
        node("child", Some("parent"), &none, true),
    ]);

    assert!(roots.is_empty());
}

/// A descendant belongs to its nearest tagged ancestor, so a nested root owns
/// its own subtree instead of the outer root owning it too.
#[test]
fn a_descendant_is_attributed_to_its_nearest_tagged_ancestor() {
    let tags = epic_tags();
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("outer", None, &tags, false),
        node("inner", Some("outer"), &tags, false),
        node("inner-child", Some("inner"), &none, true),
    ]);

    assert_eq!(roots.get("inner"), Some(&vec!["inner-child"]));
    assert_eq!(roots.get("outer"), None);
}

/// A malformed store must cost a bounded walk, not a hang.
#[test]
fn a_parent_cycle_terminates_without_naming_a_root() {
    let tags = epic_tags();
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([
        node("root", None, &tags, false),
        node("left", Some("right"), &none, true),
        node("right", Some("left"), &none, true),
    ]);

    assert!(roots.is_empty());
}

/// A parent the caller did not supply ends the walk: an unresolvable chain
/// cannot be shown to reach a tagged root, so nothing is withheld on it.
#[test]
fn a_missing_parent_ends_the_walk() {
    let none: Vec<String> = Vec::new();

    let roots = inherited_only_epic_roots([node("child", Some("absent"), &none, true)]);

    assert!(roots.is_empty());
}

#[test]
fn the_tag_predicate_matches_the_exact_tag_only() {
    assert!(has_epic_tag(&epic_tags()));
    assert!(!has_epic_tag(&["epic-ish".to_string()]));
    assert!(!has_epic_tag(&[]));
}
