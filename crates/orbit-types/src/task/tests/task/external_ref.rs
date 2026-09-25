use crate::task::TaskError;
use crate::task::{ExternalRef, push_external_ref_if_missing};

#[test]
fn external_ref_try_new_normalizes_valid_input() {
    let external_ref = ExternalRef::try_new(
        " jira ".to_string(),
        " ENG-1234 ".to_string(),
        Some(" https://example.com/browse/ENG-1234 ".to_string()),
    )
    .expect("valid external ref");

    assert_eq!(external_ref.system, "jira");
    assert_eq!(external_ref.id, "ENG-1234");
    assert_eq!(
        external_ref.url.as_deref(),
        Some("https://example.com/browse/ENG-1234")
    );
}

#[test]
fn external_ref_rejects_invalid_system() {
    let error = ExternalRef::try_new("Jira".to_string(), "ENG-1234".to_string(), None).unwrap_err();

    assert!(matches!(error, TaskError::Invalid(_)));
    assert!(error.to_string().contains("must match"));
}

#[test]
fn external_ref_validate_system_normalizes_valid_input() {
    assert!(ExternalRef::is_valid_system(" jira "));
    assert_eq!(
        ExternalRef::validate_system(" github-pr ").expect("valid system"),
        "github-pr"
    );
    assert!(ExternalRef::validate_system("GitHub").is_err());
}

#[test]
fn external_ref_rejects_empty_id() {
    let error = ExternalRef::try_new("jira".to_string(), "   ".to_string(), None).unwrap_err();

    assert!(matches!(error, TaskError::Invalid(_)));
    assert!(error.to_string().contains("id must not be empty"));
}

#[test]
fn external_ref_rejects_invalid_url() {
    let error = ExternalRef::try_new(
        "jira".to_string(),
        "ENG-1234".to_string(),
        Some("not a url".to_string()),
    )
    .unwrap_err();

    assert!(matches!(error, TaskError::Invalid(_)));
    assert!(error.to_string().contains("valid URL"));
}

#[test]
fn external_ref_deserialization_uses_validator() {
    let error = serde_json::from_value::<ExternalRef>(serde_json::json!({
        "system": "jira",
        "id": "ENG-1234",
        "url": "not a url"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("valid URL"));
}

#[test]
fn push_external_ref_if_missing_is_idempotent_by_key() {
    let mut refs = vec![ExternalRef::github_pr("42").expect("github pr ref")];

    push_external_ref_if_missing(
        &mut refs,
        ExternalRef::github_pr("42").expect("duplicate github pr ref"),
    );
    push_external_ref_if_missing(
        &mut refs,
        ExternalRef::parse_key("jira:ENG-1234").expect("jira ref"),
    );

    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].system, "github-pr");
    assert_eq!(refs[0].id, "42");
    assert_eq!(refs[1].system, "jira");
    assert_eq!(refs[1].id, "ENG-1234");
}
