#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use serde_json::json;
use tempfile::tempdir;

const INACTIVE_TOOL_NAMES: &[&str] = &[
    // Candidate inspection remains on the CLI surface.
    "orbit.auto_task.show",
    "orbit.friction.show",
    "orbit.friction.tags",
    "orbit.task.locks",
    "orbit.task.locks.release",
    "orbit.task.locks.reserve",
    "orbit.friction.stats",
];

/// An `orbit` invocation pinned to the fixture's own home, with no inherited
/// authority.
///
/// ORB-11300: clearing `ORBIT_ROOT` alone left the inherited
/// `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair in place, and that pair
/// outranks `HOME` — a suite launched from inside a managed Orbit run listed
/// and initialized against the live workspace instead of this temp one.
fn orbit_at_home(work: &std::path::Path, home: &std::path::Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

/// The same invocation as [`orbit_at_home`], claiming operator capability the
/// way the denial message asks for it. A test binary is not a terminal, so the
/// chokepoint would otherwise resolve it as an unidentified caller.
fn orbit_at_home_as_operator(
    work: &std::path::Path,
    home: &std::path::Path,
) -> assert_cmd::Command {
    let mut command = orbit_at_home(work, home);
    command.env("ORBIT_OPERATOR", "1");
    command
}

#[test]
fn tool_list_all_shows_inactive_lock_reservation_with_required_input_shape() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    orbit_at_home(&work, &home)
        // `--format table` because `assert_cmd` captures through a pipe, where
        // `auto` resolves to the plain form and suppresses the header
        // (`specs/output-modes.md` §2). The STATUS *column* is what this test
        // is about, and naming the mode is how a piped caller asks for it.
        .args(["tool", "list", "--all", "--format", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("orbit.task.locks.reserve"))
        .stdout(predicate::str::contains("STATUS"))
        .stdout(predicate::str::contains("inactive"))
        .stdout(predicate::str::contains(
            "Exactly one of `task_ids` or `files`",
        ));
}

#[test]
fn tool_list_json_hides_inactive_tools_by_default() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect::<BTreeSet<_>>();

    for inactive in INACTIVE_TOOL_NAMES {
        assert!(
            !names.contains(*inactive),
            "inactive tool must not be visible through default `orbit tool list`: {inactive}"
        );
    }
}

#[test]
fn retired_docs_tools_are_absent_and_unknown_to_tool_run() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    assert!(
        tools.iter().all(|tool| {
            !tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("orbit.docs."))
        }),
        "retired orbit.docs tools must not be listed"
    );

    orbit_at_home(&work, &home)
        .args(["tool", "run", "orbit.docs.list", "--input", "{}"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("tool not found: orbit.docs.list"));
}

#[test]
fn tool_list_json_all_includes_inactive_tools_with_status() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");

    for inactive in INACTIVE_TOOL_NAMES {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == *inactive)
            .unwrap_or_else(|| panic!("inactive tool missing from --all: {inactive}"));
        assert_eq!(tool["status"], "inactive");
        assert_eq!(tool["active"], false);
    }
}

#[test]
fn tool_list_json_all_includes_parameter_schema_for_inactive_tools() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    let mint = tools
        .iter()
        .find(|tool| tool["name"] == "orbit.auto_task.mint")
        .expect("mint tool");
    assert_eq!(mint["status"], "active");
    let reserve = tools
        .iter()
        .find(|tool| tool["name"] == "orbit.task.locks.reserve")
        .expect("reserve tool");
    let parameters = reserve["parameters"].as_array().expect("parameters array");
    assert!(parameters.iter().any(|param| {
        param["name"] == "task_ids"
            && param["param_type"] == "string_list"
            && param["required"] == false
    }));
    assert!(parameters.iter().any(|param| {
        param["name"] == "files"
            && param["param_type"] == "string_list"
            && param["required"] == false
    }));
}

#[test]
fn tool_list_json_excludes_removed_task_show_context_parameters() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    let output = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let tools: Vec<serde_json::Value> = serde_json::from_slice(&output).expect("tool list JSON");
    let task_show = tools
        .iter()
        .find(|tool| tool["name"] == "orbit.task.show")
        .expect("task show tool");
    let parameters = task_show["parameters"]
        .as_array()
        .expect("parameters array");
    assert!(
        parameters
            .iter()
            .all(|param| { !matches!(param["name"].as_str(), Some("with_context" | "max_docs")) })
    );
    assert!(parameters.iter().any(|param| {
        param["name"] == "workspace"
            && param["param_type"] == "string"
            && param["required"] == false
            && param["description"]
                .as_str()
                .is_some_and(|description| description.contains("resolved globally by default"))
    }));
}

#[test]
fn tool_show_displays_lock_reservation_shapes() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    orbit_at_home(&work, &home)
        .args(["tool", "show", "orbit.task.locks.reserve"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status:"))
        .stdout(predicate::str::contains("inactive"))
        .stdout(predicate::str::contains("task_ids"))
        .stdout(predicate::str::contains("files"))
        .stdout(predicate::str::contains("optional"))
        .stdout(predicate::str::contains(
            "Exactly one of `task_ids` or `files`",
        ));
}

#[test]
fn tool_run_rejects_inactive_tools() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    orbit_at_home(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.auto_task.show",
            "--input",
            "{\"model\":\"codex\"}",
        ])
        .assert()
        .failure()
        // The JSON error payload moved to stderr [ORB-10570]; stdout carries
        // the payload and nothing else.
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("inactive"));
}

/// [ORB-12581] `orbit.drain.claims` is off the MCP surface and has no
/// subcommand of its own, so `orbit tool run` is the operator's only route to
/// it — the one both copies of the shipped `tool-surface.md` name. Registering
/// it inactive made that command fail on every surface, with a refusal that
/// no capability could clear.
#[test]
fn tool_run_reaches_the_operator_claim_listing() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(work.join(".git")).expect("create work repo");

    orbit_at_home(&work, &home)
        .args(["workspace", "init", "--name", "drain-claims-test"])
        .assert()
        .success();

    const CLAIMS: &[&str] = &["tool", "run", "orbit.drain.claims", "--input", "{}"];

    // Unidentified callers are refused by the governed-operation registry —
    // not by the agent-surface gate, which would be unclearable.
    let refused = orbit_at_home(&work, &home)
        .args(CLAIMS)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refusal = String::from_utf8_lossy(&refused);
    assert!(refusal.contains("capability denied"), "{refusal}");
    assert!(refusal.contains("operator"), "{refusal}");
    assert!(
        !refusal.contains("inactive on the agent tool surface"),
        "the operator route must not be closed by placement:\n{refusal}"
    );

    let listed = orbit_at_home_as_operator(&work, &home)
        .args(CLAIMS)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let claims: serde_json::Value = serde_json::from_slice(&listed).expect("claim listing JSON");
    assert!(
        claims.as_array().is_some_and(|claims| claims.is_empty()),
        "a fresh workspace holds no execution claims: {claims}"
    );
}

/// [ORB-12582] The owner's read-only drain surface answers `orbit tool run` on
/// the owner's own machine.
///
/// `orbit tool list` advertises both tools as active, and the CLI expresses a
/// caller's authority in the process envelope rather than in the session it
/// builds — `local_tool_session_context` carries machine identity, transport,
/// and a trace ID, and no capabilities at all. A capability read inside the
/// application function therefore refused every `orbit tool run` call while the
/// same tools answered over MCP. The floor is now a governed row, so this test
/// spawns the real binary: a session synthesized in a unit test cannot stand in
/// for the one the CLI actually sends.
#[test]
fn tool_run_serves_the_owner_local_read_only_drain_surface() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(work.join(".git")).expect("create work repo");

    orbit_at_home(&work, &home)
        .args(["workspace", "init", "--name", "drain-read-only-test"])
        .assert()
        .success();

    const PROBE: &[&str] = &["tool", "run", "orbit.drain.probe", "--input", "{}"];
    const LOOKUP: &[&str] = &[
        "tool",
        "run",
        "orbit.drain.receipt.lookup",
        "--input",
        r#"{"request_id":"req-never-sent"}"#,
    ];

    // Both tools are advertised as active, so the surface has to serve them.
    let listed = orbit_at_home(&work, &home)
        .args(["tool", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let tools: Vec<serde_json::Value> = serde_json::from_slice(&listed).expect("tool list JSON");
    for name in ["orbit.drain.probe", "orbit.drain.receipt.lookup"] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from `orbit tool list`"));
        assert_eq!(tool["status"], "active", "{name}");
    }

    let probed = orbit_at_home_as_operator(&work, &home)
        .args(PROBE)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&probed).expect("probe JSON");
    assert_eq!(report["creates_admission_state"], false);
    assert_eq!(report["session"]["remote"], false, "{report}");
    // The reported set is the one the chokepoint resolved, not the empty set
    // the CLI's session envelope carries.
    assert!(
        report["session"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability == "operator"),
        "{report}"
    );

    let looked_up = orbit_at_home_as_operator(&work, &home)
        .args(LOOKUP)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let lookup: serde_json::Value = serde_json::from_slice(&looked_up).expect("lookup JSON");
    assert_eq!(lookup["outcome"], "not_found", "{lookup}");
    assert_eq!(lookup["grants_execution_authority"], false);

    // A follower holds `agent` and nothing more, and that is who the surface
    // exists for: the floor admits it on the CLI as it does over MCP.
    let as_agent = orbit_at_home(&work, &home)
        .env("ORBIT_AGENT_MODEL", "codex")
        .args(PROBE)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let agent_report: serde_json::Value = serde_json::from_slice(&as_agent).expect("agent probe");
    assert_eq!(agent_report["session"]["capabilities"], json!(["agent"]));

    // A caller the chokepoint cannot identify still gets no answer about the
    // owner's workspace, and hears which capability it needed.
    let refused = orbit_at_home(&work, &home)
        .args(PROBE)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refusal = String::from_utf8_lossy(&refused);
    assert!(refusal.contains("capability denied"), "{refusal}");
    assert!(refusal.contains("agent"), "{refusal}");
}
