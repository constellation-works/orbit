//! Shared fleet harness for the host tests: real Orbit installations, each
//! its own `$HOME`, reached through a fake `ssh` on PATH.
//!
//! The fake `ssh` runs the exact remote argv the federated client composes
//! (`orbit mcp serve --remote-caller-machine-id …`) under the routed install's
//! home with the built binary. A target missing from the routing map fails the
//! way an unresolvable host does; a routing mode can strip or alter the
//! remote's discovery envelope to stand in for an older or skewed build. Every
//! target reached is appended to `routes.calls`, and every remote command, with
//! its target, to `routes.argv`.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::git_repo;

/// One isolated installation: its own `$HOME`, identity and checkouts.
pub(crate) struct Install {
    pub(crate) home: PathBuf,
    pub(crate) machine_id: String,
}

impl Install {
    /// The checkout its workspace was initialized in.
    pub(crate) fn repo(&self) -> PathBuf {
        self.home.join("repo")
    }
}

pub(crate) struct Fleet {
    pub(crate) temp: TempDir,
    pub(crate) bin: PathBuf,
    pub(crate) routes: PathBuf,
    pub(crate) local: Install,
    pub(crate) local_repo: PathBuf,
}

impl Fleet {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn install(&self, dir: &str, machine_name: &str, prefix: &str) -> Install {
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
    pub(crate) fn route(&self, target: &str, install: &Install, mode: &str) {
        let mut routes: Value =
            serde_json::from_str(&fs::read_to_string(&self.routes).expect("read routes"))
                .expect("parse routes");
        routes[target] = json!({ "home": install.home, "mode": mode });
        fs::write(&self.routes, routes.to_string()).expect("write routes");
    }

    pub(crate) fn unroute(&self, target: &str) {
        let mut routes: Value =
            serde_json::from_str(&fs::read_to_string(&self.routes).expect("read routes"))
                .expect("parse routes");
        routes
            .as_object_mut()
            .expect("routes object")
            .remove(target);
        fs::write(&self.routes, routes.to_string()).expect("write routes");
    }

    pub(crate) fn orbit_in(&self, home: &Path, cwd: &Path, args: &[&str]) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.process(home, cwd, args))
    }

    /// The same invocation as a plain process, for a caller that keeps it
    /// running (an MCP server it talks to over stdio).
    pub(crate) fn process(&self, home: &Path, cwd: &Path, args: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
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

    pub(crate) fn orbit(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        self.orbit_in(&self.local.home, cwd, args)
            .output()
            .expect("spawn orbit")
    }

    pub(crate) fn orbit_ok(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        let output = self.orbit(cwd, args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub(crate) fn json(&self, args: &[&str]) -> Value {
        let output = self.orbit_ok(&self.local.home, args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?} JSON: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    /// A successful command's stdout, piped as a script would read it.
    pub(crate) fn text(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.orbit_ok(&self.local.home, args).stdout).into_owned()
    }

    /// Run a command expected to fail and return its JSON error code and
    /// message.
    pub(crate) fn refused(&self, args: &[&str]) -> (String, String) {
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

    pub(crate) fn hosts_file(&self) -> PathBuf {
        self.local.home.join(".orbit/hosts.toml")
    }

    pub(crate) fn legacy_file(&self) -> PathBuf {
        self.local.home.join(".orbit/mcp-destinations.toml")
    }

    pub(crate) fn hosts_bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.hosts_file()).ok()
    }

    pub(crate) fn doctor_hosts_row(&self) -> Value {
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
with open({calls}, 'a') as calls:
    calls.write(target + '\n')
with open({argv}, 'a') as log:
    log.write(target + '\t' + command + '\n')
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
        calls = json!(routes.with_extension("calls")),
        argv = json!(routes.with_extension("argv")),
        binary = json!(env!("CARGO_BIN_EXE_orbit")),
    );
    fs::write(bin.join("ssh"), script).expect("write fake ssh");
    fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755))
        .expect("make fake ssh executable");
}

pub(crate) fn host_row<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["hosts"]
        .as_array()
        .expect("hosts array")
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} listed: {list}"))
}
