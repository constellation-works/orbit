use orbit_common::OrbitError;

use super::super::context::{ActorIdentity, ActorKind, resolve_write_actor_label};

#[test]
fn resolve_write_actor_label_prefers_canonical_family() {
    assert_eq!(
        resolve_write_actor_label("human:qa", None, Some("gpt-5.5")).expect("normalize"),
        "codex"
    );
    assert_eq!(
        resolve_write_actor_label("human:qa", None, Some("claude")).expect("family"),
        "claude"
    );
    assert_eq!(
        resolve_write_actor_label("human:qa", None, None).expect("process actor"),
        "human:qa"
    );
}

#[test]
fn resolve_write_actor_label_refuses_unrecognized_model() {
    let error =
        resolve_write_actor_label("human:qa", None, Some("llama")).expect_err("llama refused");
    match error {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("llama"), "{message}");
        }
        other => panic!("expected invalid_input, got {other}"),
    }
}

#[test]
fn audit_role_is_kind_not_os_user_label() {
    let human = ActorIdentity::human("human:qa");
    assert_eq!(human.kind, ActorKind::Human);
    assert_eq!(human.audit_role(), "human");
    assert_eq!(ActorIdentity::human("operator").audit_role(), "operator");
    assert_eq!(ActorIdentity::agent("codex").audit_role(), "codex");
    assert_eq!(ActorIdentity::unknown().audit_role(), "unknown");
}
