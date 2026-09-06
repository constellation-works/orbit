//! `orbit --root <ROOT> web serve` must serve `<ROOT>`'s workspace registry,
//! not the machine-global one (ORB-11388).
//!
//! Same defect family as `routine_root.rs` (ORB-11156) and `sweep_root.rs`
//! (ORB-11165): the command parsed `--root` but resolved its registry from
//! `~/.orbit/workspaces.json` regardless, so the documented isolation boundary
//! silently leaked every host workspace into the dashboard.
//!
//! The fixture registers one workspace under a custom root and a *different*
//! one under the fixture's `HOME`, then asserts `GET /api/workspaces` on a
//! server started with `--root <custom>` reports only the custom root's
//! workspace.

#![allow(missing_docs)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

const STARTUP_DEADLINE: Duration = Duration::from_secs(15);

#[test]
fn web_serve_honors_an_explicit_root_over_the_global_registry() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let custom_root = temp.path().join("custom-root");
    let custom_repo = temp.path().join("custom-repo");
    let home_repo = temp.path().join("home-repo");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&custom_repo).expect("create custom repo");
    std::fs::create_dir_all(&home_repo).expect("create home repo");
    init_git_repo(&custom_repo);
    init_git_repo(&home_repo);

    let root_arg = custom_root.to_string_lossy().into_owned();
    // A workspace registered only under the custom root ...
    orbit(
        &custom_repo,
        &home,
        &[
            "--root",
            &root_arg,
            "init",
            "--non-interactive",
            "--host-name",
            "web-root-host",
            "--task-prefix",
            "WR",
        ],
    );
    orbit(
        &custom_repo,
        &home,
        &["--root", &root_arg, "workspace", "init", "--name", "scoped"],
    );
    // ... and a different one registered only in the machine-global registry
    // this fixture's HOME stands in for.
    orbit(
        &home_repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "web-home-host",
            "--task-prefix",
            "WH",
        ],
    );
    orbit(
        &home_repo,
        &home,
        &["workspace", "init", "--name", "global-only"],
    );

    let port = free_port();
    // cwd is outside both repos, so nothing but `--root` can decide what is
    // served.
    let mut server = spawn_dashboard(temp.path(), &home, Some(&root_arg), port);
    wait_for_listening(port, &mut server);
    let served = workspace_names(port);
    stop(&mut server);

    assert_eq!(
        served,
        vec!["scoped".to_string()],
        "an explicit --root must serve only that root's registry"
    );
}

#[test]
fn web_serve_without_an_explicit_root_still_serves_the_global_registry() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);

    orbit(
        &repo,
        &home,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "web-home-host",
            "--task-prefix",
            "WH",
        ],
    );
    orbit(
        &repo,
        &home,
        &["workspace", "init", "--name", "global-only"],
    );

    let port = free_port();
    let mut server = spawn_dashboard(temp.path(), &home, None, port);
    wait_for_listening(port, &mut server);
    let served = workspace_names(port);
    stop(&mut server);

    assert_eq!(served, vec!["global-only".to_string()]);
}

// ── fixture helpers ───────────────────────────────────────────────────────

fn base_command(cwd: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    // The dashboard serves whichever registry it resolves; an inherited
    // `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair would point it at the live
    // host instead of this fixture (ORB-11300).
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn orbit(cwd: &Path, home: &Path, args: &[&str]) {
    let output = base_command(cwd, home)
        .args(args)
        .output()
        .expect("run orbit");
    assert!(
        output.status.success(),
        "orbit {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo(repo: &Path) {
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "fixture@example.com"],
        vec!["config", "user.name", "fixture"],
    ] {
        let status = Command::new("git")
            .current_dir(repo)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
}

fn spawn_dashboard(cwd: &Path, home: &Path, root: Option<&str>, port: u16) -> Child {
    let mut command = base_command(cwd, home);
    if let Some(root) = root {
        command.args(["--root", root]);
    }
    command
        .args(["web", "serve", "--port", &port.to_string(), "--no-open"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn orbit web serve")
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

fn wait_for_listening(port: u16, server: &mut Child) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if let Ok(Some(status)) = server.try_wait() {
            panic!("orbit web serve exited before listening on {port}: {status:?}");
        }
        if Instant::now() >= deadline {
            stop(server);
            panic!("orbit web serve did not listen on {port} within {STARTUP_DEADLINE:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Registered workspace names as the dashboard itself reports them, sorted.
fn workspace_names(port: u16) -> Vec<String> {
    let response = http_get(port, "/api/workspaces");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&response);
    let value: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("parse /api/workspaces body ({error}): {response}"));
    let mut names: Vec<String> = value
        .as_array()
        .expect("workspace array")
        .iter()
        .map(|workspace| {
            workspace["name"]
                .as_str()
                .expect("workspace name")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

fn stop(server: &mut Child) {
    let _ = server.kill();
    let _ = server.wait();
}
