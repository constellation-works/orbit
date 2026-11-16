use chrono::Utc;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{POST_DEPLOY_OWED_EVENT, PostDeployRecord};

#[test]
fn newest_unreadable_owed_declaration_never_resurrects_an_older_record() {
    let old = TaskHistoryEntry::new(Utc::now(), "system", POST_DEPLOY_OWED_EVENT)
        .with_note(Some(r#"{"run_id":"old","criteria":[]}"#.into()));
    for note in [
        None,
        Some("not json".into()),
        Some(r#"{"run_id":"new","criteria":[],"future":true}"#.into()),
    ] {
        let history = vec![
            old.clone(),
            TaskHistoryEntry::new(Utc::now(), "system", POST_DEPLOY_OWED_EVENT).with_note(note),
        ];
        assert!(
            PostDeployRecord::read(&history).is_err(),
            "unreadable newest must not expose the old all-met record"
        );
    }
}

#[test]
fn a_newest_owed_declaration_from_a_non_automation_actor_is_refused() {
    let note = Some(r#"{"run_id":"delivery","criteria":[]}"#.to_string());
    let history = vec![
        TaskHistoryEntry::new(Utc::now(), "system", POST_DEPLOY_OWED_EVENT).with_note(note.clone()),
        TaskHistoryEntry::new(Utc::now(), "human:fixture", POST_DEPLOY_OWED_EVENT).with_note(note),
    ];
    assert!(
        PostDeployRecord::read(&history).is_err(),
        "an operator measurement may not replace the owner's owed declaration"
    );
}
