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
}

#[test]
fn normalized_limit_defaults_zero_and_caps_oversized_requests() {
    let limit = |limit| {
        GlobalSearchParams {
            limit,
            ..Default::default()
        }
        .normalized_limit()
    };

    assert_eq!(limit(0), DEFAULT_LIMIT);
    assert_eq!(limit(7), 7);
    assert_eq!(limit(u32::MAX as usize), MAX_LIMIT);
}
