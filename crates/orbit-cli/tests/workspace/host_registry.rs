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
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::git_repo;

/// One isolated installation: its own `$HOME`, identity and checkouts.
struct Install {
    home: PathBuf,
    machine_id: String,
}

struct Fleet {
    temp: TempDir,
    bin: PathBuf,
    routes: PathBuf,
    local: Install,
    local_repo: PathBuf,
}

impl Fleet {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin).expect("create stub bin");
        let routes = temp.path().join("ssh-routes.json");
        fs::write(&routes, "{}").expect("write empty routes");
        install_fake_ssh(&bin, &routes);
        let local_home = temp.path().join("local-home");
        let local_repo = temp.path().join("local-repo");
        let mut fleet = Self {
            local: Install {
                home: local_home.clone(),
                machine_id: String::new(),
            },
            local_repo,
            temp,
            bin,
            routes,
        };
        fleet.local = fleet.install("local-home", "local-box", "LB");
        git_repo::init(&fleet.local_repo);
        fleet.orbit_ok(
            &fleet.local_repo,
            &["workspace", "init", "--name", "local-ws"],
        );
        fleet
    }

    /// Initialize another installation with a workspace of its own.
    fn install(&self, dir: &str, machine_name: &str, prefix: &str) -> Install {
        let home = self.temp.path().join(dir);
        fs::create_dir_all(&home).expect("create install home");
        let repo = home.join("repo");
        git_repo::init(&repo);
        let run = |cwd: &Path, args: &[&str]| {
            let output = self
                .orbit_in(&home, cwd, args)
                .output()
                .expect("spawn orbit");
            assert!(
                output.status.success(),
                "{args:?} failed for {dir}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(
            &home,
            &[
                "init",
                "--non-interactive",
                "--machine-name",
                machine_name,
                "--task-prefix",
                prefix,
            ],
        );
        crate::fixture_crew::configure_sol(&home.join(".orbit"));
        run(
            &repo,
            &["workspace", "init", "--name", &format!("{dir}-ws")],
        );
        let config: toml::Value = fs::read_to_string(home.join(".orbit/config.toml"))
            .expect("read config")
            .parse()
            .expect("parse config");
        Install {
            machine_id: config["machine"]["id"]
                .as_str()
                .expect("machine.id")
                .to_string(),
            home,
        }
    }

    /// Route `target` to `install` with `mode` (`plain`, `old`, `skew`).
    fn route(&self, target: &str, install: &Install, mode: &str) {
        let mut routes: Value =
            serde_json::from_str(&fs::read_to_string(&self.routes).expect("read routes"))
                .expect("parse routes");
        routes[target] = json!({ "home": install.home, "mode": mode });
        fs::write(&self.routes, routes.to_string()).expect("write routes");
    }

    fn unroute(&self, target: &str) {
        let mut routes: Value =
            serde_json::from_str(&fs::read_to_string(&self.routes).expect("read routes"))
                .expect("parse routes");
        routes
            .as_object_mut()
            .expect("routes object")
            .remove(target);
        fs::write(&self.routes, routes.to_string()).expect("write routes");
    }

    fn orbit_in(&self, home: &Path, cwd: &Path, args: &[&str]) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(&path));
        command
            .current_dir(cwd)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("PATH", std::env::join_paths(paths).expect("join PATH"))
            .args(args);
        command
    }

    fn orbit(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        self.orbit_in(&self.local.home, cwd, args)
            .output()
            .expect("spawn orbit")
    }

    fn orbit_ok(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        let output = self.orbit(cwd, args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.orbit_ok(&self.local.home, args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?} JSON: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    /// Run a command expected to fail and return its JSON error code and
    /// message.
    fn refused(&self, args: &[&str]) -> (String, String) {
        // `--json` goes right after the subcommand so it stays an option even
        // when the remaining arguments follow a `--`.
        let mut args = args.to_vec();
        args.insert(2, "--json");
        let output = self.orbit(&self.local.home, &args);
        assert!(!output.status.success(), "{args:?} must be refused");
        let error: Value = serde_json::from_slice(&output.stdout)
            .or_else(|_| serde_json::from_slice(&output.stderr))
            .unwrap_or_else(|_| {
                panic!(
                    "{args:?} error JSON: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        (
            error["code"].as_str().unwrap_or_default().to_string(),
            error["error"].as_str().unwrap_or_default().to_string(),
        )
    }

    fn hosts_file(&self) -> PathBuf {
        self.local.home.join(".orbit/hosts.toml")
    }

    fn legacy_file(&self) -> PathBuf {
        self.local.home.join(".orbit/mcp-destinations.toml")
    }

    fn hosts_bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.hosts_file()).ok()
    }

    fn doctor_hosts_row(&self) -> Value {
        let output = self.orbit(&self.local_repo, &["doctor", "--json"]);
        let rows: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "doctor JSON: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        rows.as_array()
            .expect("doctor rows")
            .iter()
            .find(|row| row["check"] == "hosts" || row["check_name"] == "hosts")
            .cloned()
            .unwrap_or_else(|| panic!("doctor has a hosts row: {rows}"))
    }
}

/// A fake `ssh -T -- <target> <remote argv>` that runs the remote argv with
/// the built binary under the routed install's `$HOME`.
fn install_fake_ssh(bin: &Path, routes: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        r#"#!/usr/bin/env python3
import json, os, shlex, subprocess, sys
args = sys.argv[1:]
split = args.index('--')
target, command = args[split + 1], args[split + 2]
host = json.load(open({routes})).get(target)
if host is None:
    sys.stderr.write('ssh: Could not resolve hostname %s\n' % target)
    sys.exit(255)
argv = shlex.split(command)
assert argv.pop(0) == 'orbit'
env = dict(os.environ, HOME=host['home'], USERPROFILE=host['home'])
binary = {binary}
if host['mode'] == 'plain':
    os.chdir(host['home'])
    os.execve(binary, [binary] + argv, env)
child = subprocess.Popen([binary] + argv, cwd=host['home'], env=env, stdout=subprocess.PIPE)
for line in child.stdout:
    try:
        message = json.loads(line)
    except ValueError:
        message = None
    content = message.get('result', {{}}).get('structuredContent') if isinstance(message, dict) else None
    if isinstance(content, dict) and 'machine_id' in content and 'workspaces' in content:
        if host['mode'] == 'old':
            for key in ('machine_name', 'task_prefix', 'binary_version', 'protocol_fingerprint'):
                content.pop(key, None)
        elif host['mode'] == 'skew':
            content['binary_version'] = '0.0.1-skewed'
        line = (json.dumps(message) + '\n').encode()
    sys.stdout.buffer.write(line)
    sys.stdout.buffer.flush()
sys.exit(child.wait())
"#,
        routes = json!(routes),
        binary = json!(env!("CARGO_BIN_EXE_orbit")),
    );
    fs::write(bin.join("ssh"), script).expect("write fake ssh");
    fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755))
        .expect("make fake ssh executable");
}

fn host_row<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["hosts"]
        .as_array()
        .expect("hosts array")
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} listed: {list}"))
}

#[test]
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
    let parsed: toml::Value = written.parse().expect("hosts.toml parses");
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
        (&["host", "add", "echo"][..], "host_is_local"),
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
    assert!(
        written.find("name = \"alpha\"") < written.find("name = \"build-box\""),
        "entries are written sorted by name: {written}"
    );
}

#[test]
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
    let human =
        String::from_utf8_lossy(&fleet.orbit_ok(&fleet.local.home, &["host", "list"]).stdout)
            .into_owned();
    assert!(
        human.contains("local-box [local]") && human.contains(&alpha.machine_id),
        "human list marks the local host and shows every entry: {human}"
    );

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
    let alpha_row = host_row(&list, "alpha");
    assert_eq!(
        alpha_row["error"]["code"], "host_identity_mismatch",
        "{alpha_row}"
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
    let (code, _) = fleet.refused(&["host", "rename", "primary", "BETA"]);
    assert_eq!(code, "host_name_conflict");
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

#[test]
fn legacy_destinations_migrate_on_first_mutation_and_both_files_are_refused() {
    let fleet = Fleet::new();
    let alpha = fleet.install("alpha", "alpha", "AL");
    let beta = fleet.install("beta", "beta", "BE");
    fleet.route("alpha", &alpha, "plain");
    fleet.route("beta", &beta, "plain");
    let legacy = format!(
        "[[destinations]]\nssh = \"alpha\"\nmachine_id = \"{}\"\n",
        alpha.machine_id
    );
    fs::write(fleet.legacy_file(), &legacy).expect("write legacy file");

    let list = fleet.json(&["host", "list", "--json"]);
    assert_eq!(list["legacy"], true, "{list}");
    let alpha_row = host_row(&list, "alpha");
    assert_eq!(alpha_row["legacy"], true, "{alpha_row}");
    assert_eq!(alpha_row["task_prefix"], "AL", "probed live: {alpha_row}");
    let doctor = fleet.doctor_hosts_row();
    assert_eq!(doctor["status"], "warning", "legacy-only warns: {doctor}");

    // A legacy row that does not answer refuses the migration, touching
    // neither file.
    fleet.unroute("alpha");
    let (code, message) = fleet.refused(&["host", "add", "beta"]);
    assert_eq!(code, "legacy_host_unreachable", "{message}");
    assert!(fleet.hosts_bytes().is_none(), "no host file was written");
    assert_eq!(
        fs::read_to_string(fleet.legacy_file()).ok(),
        Some(legacy.clone())
    );

    fleet.route("alpha", &alpha, "plain");
    let added = fleet.json(&["host", "add", "beta", "--json"]);
    assert_eq!(added["migrated"][0]["name"], "alpha", "{added}");
    assert_eq!(added["migrated"][0]["task_prefix"], "AL", "{added}");
    assert!(!fleet.legacy_file().exists(), "the legacy file is retired");
    let list = fleet.json(&["host", "list", "--no-probe", "--json"]);
    assert_eq!(list["legacy"], false, "{list}");
    assert_eq!(host_row(&list, "alpha")["legacy"], false);
    assert_eq!(host_row(&list, "beta")["task_prefix"], "BE");

    // Both files at once are two answers to one question.
    fs::write(fleet.legacy_file(), &legacy).expect("restore legacy file");
    let (code, message) = fleet.refused(&["host", "list"]);
    assert_eq!(code, "host_file_conflict");
    assert!(
        message.contains("hosts.toml") && message.contains("mcp-destinations.toml"),
        "both paths are named: {message}"
    );
    let doctor = fleet.doctor_hosts_row();
    assert_eq!(doctor["status"], "error", "{doctor}");
    fs::remove_file(fleet.legacy_file()).expect("remove legacy file");

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
