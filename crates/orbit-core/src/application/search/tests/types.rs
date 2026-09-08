use super::*;
use serde_json::json;

#[test]
fn empty_whitespace_query_note_requires_two_tokens() {
    assert!(empty_whitespace_query_note("EnvGuard").is_none());
    assert!(empty_whitespace_query_note("   ").is_none());
    let note = empty_whitespace_query_note("EnvGuard workspace_init").expect("multi-word");
    assert!(note.contains("single case-insensitive substring"));
    assert!(note.contains("not proof the corpus is empty"));
    assert!(note.contains("EnvGuard, workspace_init"));
}

#[test]
fn search_modes_serialize_with_public_flag_names() {
    assert_eq!(
        serde_json::to_value(GlobalSearchMode::Lexical).expect("serialize mode"),
        json!("lexical")
    );
    assert_eq!(
        serde_json::to_value(GlobalSearchMode::Hybrid).expect("serialize mode"),
        json!("hybrid")
    );
    assert_eq!(
        serde_json::to_value(GlobalSearchMode::Neighbor).expect("serialize mode"),
        json!("neighbor")
    );
}
