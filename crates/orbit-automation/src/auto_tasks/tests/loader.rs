use std::fs;

use tempfile::tempdir;

use crate::auto_tasks::loader::{auto_tasks_dir, collect_auto_tasks};

#[test]
fn rejects_definition_with_invalid_cron_during_collection() {
    let root = tempdir().expect("temporary definition root");
    let definitions = auto_tasks_dir(root.path());
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    fs::write(
        definitions.join("invalid-cron.yaml"),
        r#"schemaVersion: 1
name: invalid-cron
schedule:
  cron: "every day"
template:
  title: Invalid cron fixture
"#,
    )
    .expect("definition fixture");

    let collection = collect_auto_tasks(root.path());

    assert!(collection.definitions.is_empty());
    assert_eq!(collection.errors.len(), 1);
    assert!(
        collection.errors[0]
            .message
            .contains("invalid cron expression 'every day'")
    );
}
