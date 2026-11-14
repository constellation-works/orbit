#![cfg(unix)]
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[cfg(unix)]
#[test]
fn external_tool_cli_lifecycle_executes_fixture_and_preserves_builtin_catalog() {
    let fixture = Fixture::new();
    let script_path = fixture.repo.join("fixture_tool.py");
    let script = script_path.to_str().unwrap().to_string();
    let name = "qa.local_echo";
    write_fixture_tool(&script_path, name);
    let script = script.as_str();
    let script_bytes = fs::read(script).unwrap();
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

/// Write the executable and v1 sidecar the removed `orbit tool scaffold`
/// used to generate, so the lifecycle below covers registration of an
/// existing sidecar tool (`orbit tool add` keeps that path working).
#[cfg(unix)]
fn write_fixture_tool(script_path: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        "#!/usr/bin/env python3\n\
         import json\n\
         import os\n\
         import sys\n\
         \n\
         \n\
         def main():\n\
         \x20   payload = json.load(sys.stdin)\n\
         \x20   subject = payload.get(\"name\") or \"Fixture\"\n\
         \x20   response = {{\"ok\": True, \"tool\": \"{name}\", \"message\": f\"Hello, {{subject}}!\"}}\n\
         \x20   if payload.get(\"include_context\"):\n\
         \x20       response[\"context\"] = {{\"workspace_root\": os.environ.get(\"ORBIT_TOOL_WORKSPACE_ROOT\")}}\n\
         \x20   json.dump(response, sys.stdout)\n\
         \x20   sys.stdout.write(\"\\n\")\n\
         \x20   return 0\n\
         \n\
         \n\
         raise SystemExit(main())\n"
    );
    fs::write(script_path, script).unwrap();
    fs::set_permissions(script_path, fs::Permissions::from_mode(0o755)).unwrap();

    let sidecar = script_path
        .with_extension("")
        .with_extension("orbit-tool.yaml");
    let manifest = format!(
        "schemaVersion: 1\n\
         name: {name}\n\
         description: Fixture echo tool.\n\
         parameters:\n\
         \x20 - name: name\n\
         \x20   description: Greeting target echoed back by the fixture tool.\n\
         \x20   param_type: string\n\
         \x20   required: false\n\
         \x20 - name: include_context\n\
         \x20   description: When true, include workspace context in the response.\n\
         \x20   param_type: boolean\n\
         \x20   required: false\n"
    );
    fs::write(&sidecar, manifest).unwrap();
}
