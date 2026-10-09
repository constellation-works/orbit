//! Operator settings on bundled auto-tasks stay outside the managed body, so
//! the body keeps refreshing; settings-only forks migrate back under
//! management, and body forks are preserved and reported [ORB-14909].

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::security::release::sha256_hex;
use serde_json::Value;

use crate::isolated_cli_fixture::Fixture;

const SETTINGS_FILE: &str = ".orbit-auto-task-settings.json";
const MANIFEST_FILE: &str = ".orbit-managed-assets.json";

fn definition_path(fixture: &Fixture, name: &str) -> PathBuf {
    let shown = fixture.json(&["auto-task", "show", name, "--json"]);
    PathBuf::from(shown["definition_source"]["path"].as_str().unwrap())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Rewrite the managed manifest's digest for `name`; `None` drops the entry,
/// leaving the file as untracked as an older hand-merged fork.
fn set_manifest_digest(dir: &Path, name: &str, digest: Option<String>) {
    let path = dir.join(MANIFEST_FILE);
    let mut manifest = read_json(&path);
    let assets = manifest["assets"].as_object_mut().unwrap();
    match digest {
        Some(digest) => assets.insert(name.to_string(), Value::String(digest)),
        None => assets.remove(name),
    };
    fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

fn sync_outcome(sync: &Value, name: &str) -> String {
    sync["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["kind"] == "auto_task" && action["name"] == name)
        .map(|action| action["outcome"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn auto_task_doctor_row(fixture: &Fixture) -> Value {
    // Other checks may fail in a disposable home; only the row matters here.
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor JSON: {error}; {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    rows.into_iter()
        .find(|row| row["check"] == "artifacts-auto-tasks")
        .expect("artifacts-auto-tasks row")
}

#[test]
fn bundled_auto_task_settings_stay_outside_the_body_and_survive_a_body_refresh() {
    let fixture = Fixture::new();
    let path = definition_path(&fixture, "security-review");
    let dir = path.parent().unwrap().to_path_buf();
    let bundled = fs::read(&path).unwrap();

    fixture.json(&["auto-task", "toggle", "security-review", "on", "--json"]);
    fixture.json(&[
        "auto-task",
        "update",
        "security-review",
        "--crew",
        "opus",
        "--json",
    ]);

    assert_eq!(
        fs::read(&path).unwrap(),
        bundled,
        "settings edits must leave the managed body byte-identical"
    );
    let settings = read_json(&dir.join(SETTINGS_FILE));
    let entry = &settings["definitions"]["security-review"];
    assert_eq!(entry["enabled"], true);
    assert_eq!(entry["crew"], "opus");
    let shown = fixture.json(&["auto-task", "show", "security-review", "--json"]);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["crew"], "opus");
    assert_eq!(shown["layering"]["body"], "managed");

    // Simulate an upgrade: the on-disk body and its recorded digest belong to
    // an older release, so the current bundled body is a new rendered digest.
    let older =
        String::from_utf8(bundled.clone())
            .unwrap()
            .replacen("title: ", "title: Older release ", 1);
    fs::write(&path, &older).unwrap();
    set_manifest_digest(&dir, "security-review", Some(sha256_hex(older.as_bytes())));

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "security-review"), "refreshed");
    assert_eq!(fs::read(&path).unwrap(), bundled);

    let shown = fixture.json(&["auto-task", "show", "security-review", "--json"]);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["crew"], "opus");
    let minted = fixture.json(&["auto-task", "mint", "security-review", "--json"]);
    assert_eq!(
        minted["crew"], "opus",
        "settings apply to the next minted task"
    );
}

#[test]
fn settings_only_fork_migrates_and_a_body_fork_is_preserved_and_reported() {
    let fixture = Fixture::new();
    let settings_fork = definition_path(&fixture, "doc-duties");
    let dir = settings_fork.parent().unwrap().to_path_buf();
    let bundled = fs::read_to_string(&settings_fork).unwrap();

    // The shape the on-call operator found: an untracked file that enabled
    // the default and re-crewed it, colliding with the bundled name.
    let forked = bundled
        .replacen("enabled: false", "enabled: true", 1)
        .replacen("crew: system", "crew: opus", 1);
    assert_ne!(forked, bundled);
    fs::write(&settings_fork, &forked).unwrap();
    set_manifest_digest(&dir, "doc-duties", None);

    let before = auto_task_doctor_row(&fixture);
    assert_eq!(before["status"], "warning");
    let message = before["message"].as_str().unwrap();
    assert!(message.contains("`doc-duties`"), "{message}");
    assert!(message.contains("stale"), "{message}");

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "doc-duties"), "migrated");
    assert_eq!(fs::read_to_string(&settings_fork).unwrap(), bundled);
    let shown = fixture.json(&["auto-task", "show", "doc-duties", "--json"]);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["crew"], "opus");
    assert_eq!(shown["layering"]["body"], "managed");
    assert_eq!(auto_task_doctor_row(&fixture)["status"], "ok");

    // A changed acceptance criterion is a body edit: preserved, not migrated.
    let body_fork = definition_path(&fixture, "code-review");
    let original = fs::read_to_string(&body_fork).unwrap();
    let edited = original
        .replacen(
            "acceptance_criteria:\n  - ",
            "acceptance_criteria:\n  - Locally reworded. ",
            1,
        )
        .replacen("enabled: false", "enabled: true", 1);
    assert_ne!(edited, original, "fixture must change a criterion");
    fs::write(&body_fork, &edited).unwrap();

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "code-review"), "preserved");
    assert_eq!(fs::read_to_string(&body_fork).unwrap(), edited);

    let shown = fixture.json(&["auto-task", "show", "code-review", "--json"]);
    assert_eq!(shown["layering"]["body"], "forked");
    assert_eq!(
        shown["layering"]["forked_fields"],
        serde_json::json!(["template.acceptance_criteria"])
    );
    assert_eq!(
        shown["layering"]["settings_fields"],
        serde_json::json!(["enabled"])
    );

    let row = auto_task_doctor_row(&fixture);
    assert_eq!(row["status"], "warning");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("forked: `code-review`"), "{message}");
    assert!(
        message.contains("template.acceptance_criteria"),
        "{message}"
    );
    let remediation = row["remediation"].as_str().unwrap();
    assert!(
        !remediation.contains("Move") && !remediation.contains("rename"),
        "a body fork's remedy must not discard its settings by moving the file: {remediation}"
    );
}
