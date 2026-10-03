#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

#[path = "support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[test]
fn auto_task_cli_delete_opt_out_restore_and_open_task_refusals_persist() {
    let fixture = Fixture::new();
    let shipped = fixture.json(&["auto-task", "show", "backlog-hygiene", "--json"]);
    let shipped_path = PathBuf::from(shipped["definition_source"]["path"].as_str().unwrap());
    let original = fs::read(&shipped_path).unwrap();
    let removed = fixture.json(&[
        "auto-task",
        "delete",
        "backlog-hygiene",
        "--reason",
        "Disposable fixture opt-out",
        "--json",
    ]);
    assert_eq!(removed["opted_out"], true);
    assert!(!shipped_path.exists());
    fixture
        .command(&["auto-task", "show", "backlog-hygiene", "--json"])
        .assert()
        .failure();
    fixture
        .command(&["workspace", "sync", "--json"])
        .assert()
        .success();
    assert!(
        !shipped_path.exists(),
        "sync must honor the recorded opt-out"
    );
    let restored = fixture.json(&["auto-task", "restore", "backlog-hygiene", "--json"]);
    assert_eq!(restored["enabled"], false);
    assert_eq!(fs::read(&shipped_path).unwrap(), original);
    assert_eq!(
        fixture.json(&["auto-task", "show", "backlog-hygiene", "--json"])["enabled"],
        false
    );

    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        "fixture-open-task",
        "--every-minutes",
        "60",
        "--title",
        "Fixture minted task",
        "--json",
    ]);
    let minted = fixture.json(&["auto-task", "mint", "fixture-open-task", "--json"]);
    let id = minted["id"].as_str().unwrap();
    let task_before = fixture.json(&["task", "show", id, "--json"]);
    let definition_before = fixture.json(&["auto-task", "show", "fixture-open-task", "--json"]);
    fixture
        .command(&[
            "auto-task",
            "delete",
            "fixture-open-task",
            "--reason",
            "Refused while open",
            "--json",
        ])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["auto-task", "show", "fixture-open-task", "--json"]),
        definition_before
    );
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), task_before);
    let forced = fixture.json(&[
        "auto-task",
        "delete",
        "fixture-open-task",
        "--reason",
        "Explicit disposable force",
        "--force",
        "--json",
    ]);
    assert_eq!(forced["opted_out"], false);
    assert!(
        forced["open_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == id)
    );
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), task_before);
    fixture
        .command(&["auto-task", "restore", "fixture-open-task", "--json"])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), task_before);
}
