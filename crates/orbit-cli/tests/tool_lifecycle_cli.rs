#![cfg(unix)]
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

#[path = "support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[cfg(unix)]
#[test]
fn external_tool_cli_lifecycle_executes_fixture_and_preserves_builtin_catalog() {
    let fixture = Fixture::new();
    let script = fixture.repo.join("fixture_tool.py");
    let script = script.to_str().unwrap();
    let name = "qa.local_echo";
    let scaffold = fixture.json(&[
        "tool", "scaffold", script, "--name", name, "--format", "json",
    ]);
    assert_eq!(scaffold["tool"], name);
    let script_bytes = fs::read(script).unwrap();
    fixture
        .command(&["tool", "scaffold", script, "--name", name])
        .assert()
        .failure();
    assert_eq!(fs::read(script).unwrap(), script_bytes);
    let added = fixture.json(&["tool", "add", script, "--format", "json"]);
    assert_eq!(added["tool"], name);
    let show = fixture.json(&["tool", "show", name, "--format", "json"]);
    assert_eq!(show["name"], name);
    assert_eq!(show["builtin"], false);
    assert_eq!(show["enabled"], true);
    assert!(
        show["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "name")
    );
    let input = r#"{"name":"Fixture","include_context":true}"#;
    let output = fixture.json(&["tool", "run", name, "--input", input, "--format", "json"]);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], name);
    assert_eq!(output["message"], "Hello, Fixture!");
    assert_eq!(
        PathBuf::from(output["context"]["workspace_root"].as_str().unwrap()),
        fixture.repo.canonicalize().unwrap()
    );
    fixture.json(&["tool", "disable", name, "--format", "json"]);
    assert_eq!(
        fixture.json(&["tool", "show", name, "--format", "json"])["enabled"],
        false
    );
    fixture
        .command(&["tool", "run", name, "--input", input, "--format", "json"])
        .assert()
        .failure();
    let doctor = fixture.json(&["tool", "doctor", "--format", "json"]);
    assert!(
        doctor
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["tool_name"] == name && row["status"] == "warning")
    );
    fixture.json(&["tool", "enable", name, "--format", "json"]);
    assert_eq!(
        fixture.json(&["tool", "run", name, "--input", input, "--format", "json"])["message"],
        "Hello, Fixture!"
    );
    let builtin = fixture.json(&["tool", "show", "orbit.task.list", "--format", "json"]);
    fixture
        .command(&["tool", "add", script, "--name", "orbit.task.list"])
        .assert()
        .failure();
    fixture
        .command(&["tool", "remove", "orbit.task.list"])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["tool", "show", "orbit.task.list", "--format", "json"]),
        builtin
    );
    fixture.json(&["tool", "remove", name, "--format", "json"]);
    fixture
        .command(&["tool", "show", name, "--format", "json"])
        .assert()
        .failure();
    assert!(
        !fixture
            .json(&["tool", "list", "--json", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name)
    );
    assert_eq!(
        fs::read(script).unwrap(),
        script_bytes,
        "registry removal must preserve the user's executable"
    );
}
