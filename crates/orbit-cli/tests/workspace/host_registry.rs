//! [ORB-14448] `orbit host` over the host file, against real remote Orbit
//! installations reached through a fake `ssh`.
//!
//! Each "remote" is a separately initialized `$HOME`. The fake `ssh` on PATH
//! runs the exact remote argv the federated probe composes (`orbit mcp serve
//! --remote-caller-machine-id …`) under that home with the built binary, so
//! every identity, version and protocol a test asserts is what the remote
//! itself reported. A target missing from the routing map fails the way an
//! unresolvable host does; a routing mode can strip or alter the remote's
//! discovery envelope to stand in for an older or skewed build.

use std::fs;

use serde_json::{Value, json};

use crate::git_repo;
use crate::host_fleet::{Fleet, host_row};

fn tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    fleet.route("alpha", &alpha, "plain");
    fleet.json(&["host", "add", "alpha", "--json"]);
    let root = fleet.local.home.join(".orbit");
    let absolute = fleet.orbit_ok(
        &fleet.local_repo,
        &[
            "--root",
            root.to_str().expect("UTF-8 root"),
            "host",
            "list",
            "--json",
        ],
    );
    let absolute: Value = serde_json::from_slice(&absolute.stdout).expect("absolute hosts JSON");
    assert_eq!(absolute["hosts"].as_array().expect("hosts").len(), 2);

    for use_flag in [false, true] {
        let mut command = fleet.orbit_in(&fleet.local.home, &fleet.local_repo, &[]);
        if use_flag {
            command.args(["--root", "~/.orbit"]);
        } else {
            command.env("ORBIT_ROOT", "~/.orbit");
        }
        let output = command.args(["host", "list", "--json"]).assert().success();
        let hosts: Value =
            serde_json::from_slice(&output.get_output().stdout).expect("tilde hosts JSON");
        assert_eq!(hosts, absolute);
        assert!(!fleet.local_repo.join("~").exists());
    }
}

fn host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    let beta = fleet.install("beta", "beta", "BE");
    let twin_prefix = fleet.install("twin", "twin", "AL");
    let local_prefix = fleet.install("echo", "echo", "LB");
    fleet.route("alpha", &alpha, "plain");
    fleet.route("alpha-again", &alpha, "plain");
    fleet.route("beta", &beta, "plain");
    fleet.route("beta-old", &beta, "old");
    fleet.route("twin", &twin_prefix, "plain");
    fleet.route("echo", &local_prefix, "plain");
    fleet.route("myself", &fleet.local, "plain");

    let added = fleet.json(&["host", "add", "alpha", "--json"]);
    assert_eq!(added["action"], "added", "{added}");
    assert_eq!(
        added["entry"],
        json!({
            "name": "alpha",
            "machine_id": alpha.machine_id,
            "ssh": "alpha",
            "task_prefix": "AL",
        }),
        "the entry is the remote's own identity: {added}"
    );
    assert_eq!(added["host"]["reachable"], true, "{added}");
    let written = fs::read_to_string(fleet.hosts_file()).expect("hosts.toml written");
    let parsed: toml::Value = toml::from_str(&written).expect("hosts.toml parses");
    assert_eq!(parsed["schema_version"].as_integer(), Some(1), "{written}");
    assert_eq!(
        parsed["hosts"][0]["machine_id"].as_str(),
        Some(alpha.machine_id.as_str()),
        "{written}"
    );

    let before = fleet.hosts_bytes();
    for (args, code) in [
        (&["host", "add", "alpha-again"][..], "host_exists"),
        (
            &["host", "add", "beta", "--name", "ALPHA"][..],
            "host_name_conflict",
        ),
        (
            &["host", "add", "beta", "--name", "local-box"][..],
            "host_name_conflict",
        ),
        (&["host", "add", "twin"][..], "task_prefix_conflict"),
        (&["host", "add", "myself"][..], "host_is_local"),
        (&["host", "add", "echo"][..], "task_prefix_conflict"),
        (&["host", "add", "nowhere"][..], "unreachable_destination"),
        (&["host", "add", "beta-old"][..], "host_too_old"),
        (
            &["host", "add", "--", "-oProxyCommand=evil"][..],
            "invalid_input",
        ),
    ] {
        let (refused, message) = fleet.refused(args);
        assert_eq!(refused, code, "{args:?}: {message}");
        assert!(!message.is_empty(), "{args:?} names what to do");
        if args == ["host", "add", "echo"] {
            assert!(
                message.contains(&local_prefix.machine_id) && message.contains("different machine"),
                "{message}"
            );
        }
        if args == ["host", "add", "beta", "--name", "ALPHA"] {
            assert!(message.contains("--name"), "{message}");
        }
        assert_eq!(
            fleet.hosts_bytes(),
            before,
            "{args:?} must leave hosts.toml byte-identical"
        );
    }

    let beta_added = fleet.json(&["host", "add", "beta", "--name", "build-box", "--json"]);
    assert_eq!(beta_added["entry"]["name"], "build-box", "{beta_added}");
    assert_eq!(beta_added["entry"]["task_prefix"], "BE", "{beta_added}");
    let written = fs::read_to_string(fleet.hosts_file()).expect("hosts.toml");
    let alpha_at = written.find("name = \"alpha\"");
    let build_box_at = written.find("name = \"build-box\"");
    assert!(
        matches!((alpha_at, build_box_at), (Some(alpha), Some(build_box)) if alpha < build_box),
        "both entries must be present and sorted by name: alpha={alpha_at:?} build-box={build_box_at:?}\n{written}"
    );
}

fn host_list_show_rename_and_remove_report_live_state_and_dependents() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    let beta = fleet.install("beta", "beta", "BE");
    let impostor = fleet.install("impostor", "impostor", "IM");
    fleet.route("alpha", &alpha, "plain");
    fleet.route("beta", &beta, "plain");
    fleet.json(&["host", "add", "alpha", "--json"]);
    fleet.json(&["host", "add", "beta", "--json"]);

    let list = fleet.json(&["host", "list", "--json"]);
    let rows = list["hosts"].as_array().expect("hosts");
    assert_eq!(rows[0]["local"], true, "the local host comes first: {list}");
    assert_eq!(rows[0]["name"], "local-box", "{list}");
    let local_version = rows[0]["binary_version"].clone();
    let local_protocol = rows[0]["protocol_fingerprint"].clone();
    let alpha_row = host_row(&list, "alpha");
    assert_eq!(alpha_row["reachable"], true, "{alpha_row}");
    assert_eq!(alpha_row["machine_id"], alpha.machine_id.as_str());
    assert_eq!(alpha_row["ssh"], "alpha");
    assert_eq!(alpha_row["task_prefix"], "AL");
    assert_eq!(alpha_row["binary_version"], local_version, "{alpha_row}");
    assert_eq!(alpha_row["protocol_fingerprint"], local_protocol);
    assert_eq!(alpha_row["skew"], false);
    assert_eq!(alpha_row["workspaces"][0]["role"], "owner", "{alpha_row}");
    let human = fleet.text(&["host", "list"]);
    assert!(
        human.contains("local-box [local]") && human.contains(&alpha.machine_id),
        "human list marks the local host and shows every entry: {human}"
    );

    // `--format table` and `ndjson` are honoured, not folded into the text.
    let table = fleet.text(&["host", "list", "--no-probe", "--format", "table"]);
    let header = table.lines().next().unwrap_or_default();
    assert!(
        header.contains("NAME") && header.contains("MACHINE_ID") && header.contains("REACHABLE"),
        "a table has a header row: {table}"
    );
    assert_eq!(
        table.lines().count(),
        4,
        "header and one line per host: {table}"
    );
    let ndjson = fleet.text(&["host", "list", "--no-probe", "--format", "ndjson"]);
    let names = ndjson
        .lines()
        .map(|line| {
            let record: Value = serde_json::from_str(line).expect("each line is one host");
            record["name"].as_str().unwrap_or_default().to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["local-box", "alpha", "beta"], "{ndjson}");

    // Unreachable hosts keep their cached row; a different identity behind
    // the same target fails closed with a remove-and-re-add hint; a version
    // difference is flagged.
    fleet.unroute("beta");
    fleet.route("alpha", &impostor, "plain");
    let list = fleet.json(&["host", "list", "--json"]);
    let beta_row = host_row(&list, "beta");
    assert_eq!(beta_row["reachable"], false, "{beta_row}");
    assert_eq!(beta_row["error"]["code"], "unreachable_destination");
    assert_eq!(beta_row["machine_id"], beta.machine_id.as_str());
    assert_eq!(beta_row["task_prefix"], "BE");
    let beta_error = beta_row["error"]["message"].as_str().unwrap_or_default();
    assert!(
        beta_error.starts_with("beta: ")
            && beta_error.contains("Could not resolve hostname beta")
            && !beta_error.contains(&beta.machine_id),
        "an unreachable row names its SSH target and carries ssh's reason: {beta_error}"
    );
    let alpha_row = host_row(&list, "alpha");
    assert_eq!(
        alpha_row["error"]["code"], "host_identity_mismatch",
        "{alpha_row}"
    );
    // The host answered, with the wrong identity: reachable in every
    // rendering, with the code said once.
    assert_eq!(alpha_row["reachable"], true, "{alpha_row}");
    let human = fleet.text(&["host", "list"]);
    let alpha_line = human
        .lines()
        .find(|line| line.starts_with("alpha\t"))
        .unwrap_or_else(|| panic!("alpha row: {human}"));
    let fields = alpha_line.split('\t').collect::<Vec<_>>();
    assert_eq!(fields[4], "yes", "REACHABLE matches the JSON: {alpha_line}");
    assert_eq!(
        alpha_line.matches("host_identity_mismatch").count(),
        1,
        "{alpha_line}"
    );
    let shown = fleet.text(&["host", "show", "alpha"]);
    assert!(shown.contains("  reachable:   yes\n"), "{shown}");
    assert_eq!(
        shown.matches("host_identity_mismatch").count(),
        1,
        "{shown}"
    );
    assert!(
        alpha_row["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("orbit host remove")),
        "{alpha_row}"
    );
    assert!(alpha_row["binary_version"].is_null(), "{alpha_row}");
    assert_eq!(alpha_row["machine_id"], alpha.machine_id.as_str());
    let doctor = fleet.doctor_hosts_row();
    assert_eq!(
        doctor["status"], "error",
        "identity mismatch fails doctor: {doctor}"
    );

    fleet.route("alpha", &alpha, "skew");
    fleet.route("beta", &beta, "plain");
    let list = fleet.json(&["host", "list", "--json"]);
    let alpha_row = host_row(&list, "alpha");
    assert_eq!(alpha_row["skew"], true, "{alpha_row}");
    assert_eq!(alpha_row["skew_fields"], json!(["binary_version"]));
    let doctor = fleet.doctor_hosts_row();
    assert_eq!(
        doctor["status"], "warning",
        "skew on a host not pulled from warns: {doctor}"
    );

    let offline = fleet.json(&["host", "list", "--no-probe", "--json"]);
    assert!(
        host_row(&offline, "alpha")["reachable"].is_null(),
        "{offline}"
    );

    // Show resolves by name (any case) or machine_id.
    fleet.route("alpha", &alpha, "plain");
    let shown = fleet.json(&["host", "show", "ALPHA", "--json"]);
    assert_eq!(shown["machine_id"], alpha.machine_id.as_str(), "{shown}");
    let by_id = fleet.json(&["host", "show", &alpha.machine_id, "--json"]);
    assert_eq!(by_id["name"], "alpha", "{by_id}");
    let local = fleet.json(&["host", "show", "local-box", "--json"]);
    assert_eq!(local["local"], true, "{local}");
    let (code, _) = fleet.refused(&["host", "show", "alp"]);
    assert_eq!(code, "unknown_host", "no prefix matching");

    let renamed = fleet.json(&["host", "rename", "alpha", "primary", "--json"]);
    assert_eq!(renamed["entry"]["name"], "primary", "{renamed}");
    let (code, message) = fleet.refused(&["host", "rename", "primary", "BETA"]);
    assert_eq!(code, "host_name_conflict");
    assert!(
        !message.contains("--name") && message.contains("choose another new name"),
        "rename takes the new name as an argument: {message}"
    );
    let (code, message) = fleet.refused(&["host", "rename", "local-box", "other"]);
    assert_eq!(code, "host_is_local");
    assert!(message.contains("machine.name"), "{message}");

    // A local replica checkout of the host blocks removal until --force.
    let replica = fleet.temp.path().join("replica-of-primary");
    git_repo::init(&replica);
    let init = fleet.orbit_ok(
        &replica,
        &[
            "workspace",
            "init",
            "--name",
            "primary-replica",
            "--role",
            "replica",
            "--owner",
            &alpha.machine_id,
            "--format",
            "json",
        ],
    );
    let init: Value = serde_json::from_slice(&init.stdout).expect("workspace init JSON");
    assert_eq!(
        init["owner_host"], "primary",
        "replica init names the owner's host entry: {init}"
    );
    let shown = fleet.json(&["host", "show", "primary", "--json"]);
    assert_eq!(
        shown["dependents"]["replica_checkouts"][0]["workspace_name"], "primary-replica",
        "{shown}"
    );
    let before = fleet.hosts_bytes();
    let (code, message) = fleet.refused(&["host", "remove", "primary"]);
    assert_eq!(code, "host_in_use", "{message}");
    assert!(message.contains("primary-replica"), "{message}");
    assert_eq!(fleet.hosts_bytes(), before);
    let removed = fleet.json(&["host", "remove", "primary", "--force", "--json"]);
    assert_eq!(removed["action"], "removed", "{removed}");
    assert_eq!(
        removed["orphaned"]["replica_checkouts"][0]["workspace_name"], "primary-replica",
        "{removed}"
    );
    let list = fleet.json(&["host", "list", "--no-probe", "--json"]);
    assert_eq!(list["hosts"].as_array().map(Vec::len), Some(2), "{list}");
    let (code, _) = fleet.refused(&["host", "remove", "local-box"]);
    assert_eq!(code, "host_is_local");
}

fn doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    fleet.route("alpha", &alpha, "plain");
    fleet.json(&["host", "add", "alpha", "--json"]);

    // An older build whose discovery facts lack both fields: not a match.
    fleet.route("alpha", &alpha, "old");
    let list = fleet.json(&["host", "list", "--json"]);
    let alpha_row = host_row(&list, "alpha");
    assert!(alpha_row["binary_version"].is_null(), "{alpha_row}");
    assert!(alpha_row["protocol_fingerprint"].is_null(), "{alpha_row}");
    let doctor = fleet.doctor_hosts_row();
    assert_eq!(
        doctor["status"], "warning",
        "an unknown version and protocol on a host not pulled from warns: {doctor}"
    );
    let message = doctor["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("did not report binary_version and protocol_fingerprint")
            && !message.contains("matching"),
        "{doctor}"
    );

    // Once this machine pulls from that host, pull admission refuses it.
    fleet.route("alpha", &alpha, "plain");
    let replica = fleet.temp.path().join("replica-of-alpha");
    git_repo::init(&replica);
    fleet.orbit_ok(
        &replica,
        &[
            "workspace",
            "init",
            "--name",
            "alpha-replica",
            "--role",
            "replica",
            "--owner",
            &alpha.machine_id,
            "--format",
            "json",
        ],
    );
    assert_eq!(
        fleet.doctor_hosts_row()["status"],
        "ok",
        "a matching host that is pulled from is healthy"
    );
    fleet.route("alpha", &alpha, "old");
    let doctor = fleet.doctor_hosts_row();
    assert_eq!(
        doctor["status"], "error",
        "an unknown protocol on a host pulled from fails: {doctor}"
    );
    let message = doctor["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("pull admission from it will refuse") && !message.contains("matching"),
        "{doctor}"
    );
}

fn host_file_hand_edits_are_validated_without_rewriting() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    let beta = fleet.install("beta", "beta", "BE");
    fleet.route("alpha", &alpha, "plain");
    fleet.route("beta", &beta, "plain");
    fleet.json(&["host", "add", "alpha", "--json"]);
    fleet.json(&["host", "add", "beta", "--json"]);

    // Hand edits go through the same validation on load.
    let good = fs::read_to_string(fleet.hosts_file()).expect("hosts.toml");
    for (edit, code) in [
        (
            good.replace("task_prefix = \"BE\"", "task_prefix = \"AL\""),
            "task_prefix_conflict",
        ),
        (
            good.replace("name = \"beta\"", "name = \"Alpha\""),
            "host_name_conflict",
        ),
        (
            good.replace("schema_version = 1", "schema_version = 2"),
            "invalid_input",
        ),
        (
            good.replace("ssh = \"beta\"", "ssh = \"beta\"\nport = 22"),
            "invalid_input",
        ),
    ] {
        fs::write(fleet.hosts_file(), &edit).expect("hand edit");
        let (refused, message) = fleet.refused(&["host", "list"]);
        assert_eq!(refused, code, "{message}\n{edit}");
        assert_eq!(
            fs::read_to_string(fleet.hosts_file()).ok(),
            Some(edit),
            "a refused load keeps the bytes"
        );
    }
}

#[test]
fn host_registry_contracts_0() {
    for (name, check) in [
        (
            "tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote",
            tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote as fn(),
        ),
        (
            "host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched",
            host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched as fn(),
        ),
        (
            "host_list_show_rename_and_remove_report_live_state_and_dependents",
            host_list_show_rename_and_remove_report_live_state_and_dependents as fn(),
        ),
        (
            "doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol",
            doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}

#[test]
fn host_registry_contracts_1() {
    host_file_hand_edits_are_validated_without_rewriting();
    orbit_common::test_env::record_scenario_pass(
        "host_file_hand_edits_are_validated_without_rewriting",
    );
}
