//! `orbit task add|update --not-before` through the real binary: a local
//! time is converted with the host zone (`TZ` here) and echoed in UTC and
//! local, a malformed time is refused naming it, `--clear-not-before`
//! clears it, and `orbit run ship` refuses the scheduled task with
//! `task_scheduled`.

use serde_json::Value;

use crate::isolated_cli_fixture::Fixture;

/// A zone without daylight saving, so the conversion is fixed.
const ZONE: &str = "Asia/Tokyo";

fn run(fixture: &Fixture, args: &[&str]) -> std::process::Output {
    fixture
        .command(args)
        .env("TZ", ZONE)
        .output()
        .expect("spawn orbit")
}

/// Every stored file of task `id` (bundle files carry the id in their path),
/// with its bytes.
fn stored(fixture: &Fixture, id: &str) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn walk(dir: &std::path::Path, id: &str, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, id, out);
            } else if path.to_string_lossy().contains(id) {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(&fixture.root, id, &mut files);
    walk(&fixture.repo.join(".orbit"), id, &mut files);
    files.sort();
    files
}

fn json(fixture: &Fixture, args: &[&str]) -> Value {
    let output = run(fixture, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn refusal(fixture: &Fixture, args: &[&str]) -> String {
    let output = run(fixture, args);
    assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_local_schedule_is_converted_echoed_cleared_and_refused_by_ship() {
    let fixture = Fixture::new();
    let listed = json(&fixture, &["task", "list", "--json"]);
    let untouched = listed[0]["id"].as_str().unwrap().to_string();
    let before = stored(&fixture, &untouched);
    assert!(!before.is_empty(), "the fixture task is stored");
    let added = run(
        &fixture,
        &[
            "task",
            "add",
            "--title",
            "Scheduled fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Starts after its time",
            "--not-before",
            "2099-01-01T18:00",
        ],
    );
    assert!(added.status.success(), "{added:?}");
    let id = String::from_utf8_lossy(&added.stdout).trim().to_string();
    assert!(
        String::from_utf8_lossy(&added.stderr)
            .contains("Not before: 2099-01-01T09:00:00Z (local 2099-01-01 18:00:00 +09:00)"),
        "{added:?}"
    );
    let shown = json(&fixture, &["task", "show", &id, "--json"]);
    assert_eq!(shown["not_before"], "2099-01-01T09:00:00Z", "{shown}");

    let malformed = refusal(
        &fixture,
        &["task", "update", &id, "--not-before", "next tuesday"],
    );
    assert!(malformed.contains("`next tuesday`"), "{malformed}");
    let both = refusal(
        &fixture,
        &[
            "task",
            "update",
            &id,
            "--not-before",
            "2099-01-02T09:00:00Z",
            "--clear-not-before",
        ],
    );
    assert!(both.contains("--clear-not-before"), "{both}");

    json(&fixture, &["task", "update", &id, "--approve", "--json"]);
    let ship = run(&fixture, &["run", "ship", &id, "--json"]);
    assert!(!ship.status.success(), "{ship:?}");
    let error: Value = serde_json::from_slice(&ship.stderr)
        .unwrap_or_else(|_| panic!("one JSON error: {}", String::from_utf8_lossy(&ship.stderr)));
    assert_eq!(error["code"], "task_scheduled", "{error}");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("not before 2099-01-01T09:00:00Z"),
        "{error}"
    );

    let updated = run(
        &fixture,
        &["task", "update", &id, "--not-before", "2099-02-01T09:30"],
    );
    assert!(
        String::from_utf8_lossy(&updated.stdout)
            .contains("Not before: 2099-02-01T00:30:00Z (local 2099-02-01 09:30:00 +09:00)"),
        "{updated:?}"
    );
    let cleared = json(
        &fixture,
        &["task", "update", &id, "--clear-not-before", "--json"],
    );
    assert!(cleared.get("not_before").is_none(), "{cleared}");
    for (path, bytes) in stored(&fixture, &id) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains("not_before"),
            "a cleared schedule is not written: {}",
            path.display()
        );
    }
    assert_eq!(
        stored(&fixture, &untouched),
        before,
        "a record without a schedule round-trips byte-identical"
    );
}
