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

#[test]
fn older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked() {
    let fixture = Fixture::new();
    for (name, older) in [
        (
            "run-failure-patterns",
            include_str!("../fixtures/auto-task-body-history/run-failure-patterns.yaml"),
        ),
        (
            "delivery-code-review",
            include_str!("../fixtures/auto-task-body-history/delivery-code-review.yaml"),
        ),
        (
            "security-review",
            include_str!("../fixtures/auto-task-body-history/security-review.yaml"),
        ),
    ] {
        let path = definition_path(&fixture, name);
        let bundled = fs::read_to_string(&path).unwrap();
        let dir = path.parent().unwrap();
        let mut edited = older.replacen("crew: system", "crew: opus", 1);
        if name == "delivery-code-review" {
            edited = edited
                .replacen("enabled: false", "enabled: true", 1)
                .replacen("threshold: 3", "threshold: 2", 1)
                .replacen("max_wait_minutes: 360", "max_wait_minutes: 120", 1)
                .replacen("retries: 0", "retries: 1", 1)
                .replacen(
                    "  - delivery-code-review\n",
                    "  - delivery-code-review\n  - os:linux\n",
                    1,
                );
            // CRUD reserialization drops YAML comments and changes formatting;
            // shipped-body recognition must still use the parsed body.
            let definition = orbit_common::protocol::yaml::parse_auto_task_yaml(&edited).unwrap();
            edited = serde_yaml::to_string(&definition).unwrap();
        }
        assert_ne!(edited, older, "fixture must change the crew");
        fs::write(&path, &edited).unwrap();
        // Stale operator copies can be untracked, or carry the old raw digest.
        set_manifest_digest(
            dir,
            name,
            if name == "security-review" {
                Some(sha256_hex(older.as_bytes()))
            } else {
                None
            },
        );

        let row = auto_task_doctor_row(&fixture);
        let message = row["message"].as_str().unwrap();
        assert!(
            message.contains(&format!(
                "`{name}` has a stale shipped body (will upgrade on sync)"
            )),
            "{message}"
        );
        assert!(!message.contains("body fork"), "{message}");
        let preview = fixture
            .command(&["workspace", "sync", "--check", "--json"])
            .assert()
            .code(3)
            .get_output()
            .stdout
            .clone();
        let preview: Value = serde_json::from_slice(&preview).unwrap();
        assert_eq!(sync_outcome(&preview, name), "migrated");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            edited,
            "check mode must preserve the file"
        );
        assert!(
            !dir.join(SETTINGS_FILE).exists()
                || read_json(&dir.join(SETTINGS_FILE))["definitions"][name].is_null()
        );

        let sync = fixture.json(&["workspace", "sync", "--json"]);
        assert_eq!(sync_outcome(&sync, name), "migrated");
        assert_eq!(fs::read_to_string(&path).unwrap(), bundled);
        let shown = fixture.json(&["auto-task", "show", name, "--json"]);
        assert_eq!(shown["layering"]["body"], "managed");
        assert_eq!(shown["layering"]["settings"]["crew"], "opus");
        assert_eq!(shown["template"]["crew"], "opus");
        if name == "delivery-code-review" {
            assert_eq!(shown["enabled"], true);
            let settings = &shown["layering"]["settings"];
            let trigger = &settings["schedule"]["deliveries_landed"];
            assert_eq!(trigger["threshold"], 2);
            assert_eq!(trigger["max_wait_minutes"], 120);
            assert_eq!(trigger["retries"], 1);
            assert_eq!(settings["tags"], serde_json::json!(["os:linux"]));
            assert!(
                settings["complexity"].is_null(),
                "an unchanged old default must not pin the new complexity"
            );
        }
        fixture
            .command(&["auto-task", "show", name])
            .assert()
            .success()
            .stdout(predicates::str::contains("body: managed bundled default"));
        assert_eq!(auto_task_doctor_row(&fixture)["status"], "ok");
    }

    let path = definition_path(&fixture, "run-failure-patterns");
    let old = include_str!("../fixtures/auto-task-body-history/run-failure-patterns.yaml");
    for edited in [
        old.replacen("title: ", "title: Operator body edit ", 1),
        format!("# Operator note must survive\n{old}"),
        old.replacen("  - run-failure-patterns\n", "", 1),
        old.replacen("  crew: system\n", "", 1),
    ] {
        assert_ne!(edited, old);
        fs::write(&path, &edited).unwrap();
        let row = auto_task_doctor_row(&fixture);
        let message = row["message"].as_str().unwrap();
        assert!(
            message.contains("`run-failure-patterns` is a body fork"),
            "{message}"
        );
        assert!(!message.contains("stale shipped body"), "{message}");
        let sync = fixture.json(&["workspace", "sync", "--json"]);
        assert_eq!(sync_outcome(&sync, "run-failure-patterns"), "preserved");
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
        assert_eq!(shown["layering"]["body"], "forked");
    }
}

#[test]
fn every_shipped_body_history_source_is_recognized_by_workspace_sync() {
    let fixture = Fixture::new();
    let history: Value = serde_json::from_str(include_str!(
        "../../../orbit-core/assets/auto_tasks/body-history.json"
    ))
    .unwrap();
    let sources: Value = serde_json::from_str(include_str!(
        "../fixtures/auto-task-body-history/sources.json"
    ))
    .unwrap();
    assert!(!history.as_object().unwrap().is_empty());

    for (name, records) in history.as_object().unwrap() {
        let path = definition_path(&fixture, name);
        let bundled = fs::read(&path).unwrap();
        let dir = path.parent().unwrap();
        assert!(!records.as_array().unwrap().is_empty(), "{name}");
        for record in records.as_array().unwrap() {
            let revision = record["revision"].as_str().unwrap();
            let raw = sources[name][revision]
                .as_str()
                .unwrap_or_else(|| panic!("missing shipped source: {name} at {revision}"));
            let backlog_regression = name == "backlog-hygiene" && revision.starts_with("4c84ad7a6");
            let mut variants = vec![raw.to_string()];
            if backlog_regression {
                let duplicated = raw.replacen(
                    "  - orbit.task.list\n",
                    "  - orbit.task.list\n  - orbit.task.list\n",
                    1,
                );
                assert_ne!(duplicated, raw, "required-tools regression fixture");
                variants.push(duplicated);
            }
            for raw in variants {
                // Each source starts untracked, without overrides left by a
                // previous migration. The compiled Rust classifier must prove
                // provenance rather than trusting the manifest's byte digest.
                fs::write(
                    dir.join(SETTINGS_FILE),
                    r#"{"schemaVersion":1,"definitions":{}}"#,
                )
                .unwrap();
                fs::write(&path, &raw).unwrap();
                set_manifest_digest(dir, name, None);
                if backlog_regression {
                    let row = auto_task_doctor_row(&fixture);
                    let message = row["message"].as_str().unwrap();
                    assert!(
                        message.contains("`backlog-hygiene` has a stale shipped body"),
                        "{revision}: {message}"
                    );
                    assert!(!message.contains("body fork"), "{revision}: {message}");
                }
                let sync = fixture.json(&["workspace", "sync", "--json"]);
                let outcome = sync_outcome(&sync, name);
                assert!(
                    matches!(outcome.as_str(), "migrated" | "unchanged"),
                    "shipped source must be recognized: {name} at {revision}: {outcome}"
                );
                assert_eq!(fs::read(&path).unwrap(), bundled, "{name} at {revision}");
            }
        }
    }
}

#[test]
fn older_shipped_body_keeps_explicit_settings_and_their_edit_stamp() {
    let fixture = Fixture::new();
    let path = definition_path(&fixture, "run-failure-patterns");
    let dir = path.parent().unwrap();
    let current = fs::read(&path).unwrap();
    let old = include_str!("../fixtures/auto-task-body-history/run-failure-patterns.yaml")
        .replacen("crew: system", "crew: sonnet", 1);
    fs::write(&path, old).unwrap();
    let explicit = serde_json::json!({
        "enabled": false, "crew": "system", "tags": ["operator-tag"],
        "updated_by": "human:operator", "updated_at": "2026-10-09T00:00:00Z"
    });
    fs::write(
        dir.join(SETTINGS_FILE),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "definitions": {"run-failure-patterns": explicit}
        }))
        .unwrap(),
    )
    .unwrap();

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "run-failure-patterns"), "migrated");
    assert_eq!(fs::read(&path).unwrap(), current);
    let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
    assert_eq!(shown["layering"]["settings"], explicit);
    assert_eq!(shown["template"]["crew"], "system");
    assert_eq!(shown["updated_by"], "human:operator");
    assert_eq!(shown["enabled"], false);
}

#[test]
fn older_shipped_probe_bodies_recognize_rendered_and_historical_base_branches() {
    let fixture = Fixture::new();
    fixture.json(&[
        "workspace",
        "init",
        "--name",
        "audit-qa",
        "--force",
        "--base-branch",
        "main",
        "--json",
    ]);
    fixture.json(&["workspace", "sync", "--json"]);
    let path = definition_path(&fixture, "qa-sweep");
    let bundled = fs::read(&path).unwrap();
    let old = include_str!("../fixtures/auto-task-body-history/qa-sweep.yaml");
    for branch in ["main", "agent-main", "operator-branch"] {
        let edited =
            old.replace("__ORBIT_BASE_BRANCH__", branch)
                .replacen("crew: system", "crew: opus", 1);
        fs::write(&path, &edited).unwrap();
        let sync = fixture.json(&["workspace", "sync", "--json"]);
        let shown = fixture.json(&["auto-task", "show", "qa-sweep", "--json"]);
        if branch == "operator-branch" {
            assert_eq!(sync_outcome(&sync, "qa-sweep"), "preserved");
            assert_eq!(fs::read_to_string(&path).unwrap(), edited);
            assert_eq!(shown["layering"]["body"], "forked");
        } else {
            assert_eq!(sync_outcome(&sync, "qa-sweep"), "migrated");
            assert_eq!(fs::read(&path).unwrap(), bundled);
            assert_eq!(shown["skip_if_unchanged"]["ref"], "main");
            assert_eq!(shown["layering"]["settings"]["crew"], "opus");
        }
    }
}
