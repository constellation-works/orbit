//! An `orbit` process registers as a generation participant once, however
//! many registered checkouts it opens [ORB-15674].
//!
//! Each registration stages, fsyncs and renames a record. Under drain load
//! `orbit routine list` registered once for the command and again for every
//! checkout it inspected, and the dashboard did the same for every
//! `/api/routines` computation, so routine status took tens of seconds. The
//! count is read from the registration's own debug event.

#![allow(missing_docs)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

use crate::bound_server::{Announcement, BoundServer};

const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
/// The registration event's tracing target.
const REGISTRATION_TARGET: &str = "orbit.generation.participant";

/// A home with three registered workspaces, each seeding its routines.
struct Host {
    temp: TempDir,
    home: std::path::PathBuf,
}

impl Host {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let host = Self { temp, home };
        for (index, name) in ["alpha", "beta", "gamma"].into_iter().enumerate() {
            let repo = host.temp.path().join(name);
            crate::git_repo::init(&repo);
            if index == 0 {
                host.orbit(
                    &repo,
                    &[
                        "init",
                        "--non-interactive",
                        "--machine-name",
                        "participation-host",
                        "--task-prefix",
                        "GP",
                    ],
                );
            }
            host.orbit(&repo, &["workspace", "init", "--name", name]);
        }
        host
    }

    /// `orbit` with only the registration event logged to stderr.
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("RUST_LOG", format!("off,{REGISTRATION_TARGET}=debug"))
            .env("NO_COLOR", "1");
        command
    }

    fn orbit(&self, cwd: &Path, args: &[&str]) {
        let output = self.command(cwd).args(args).output().expect("run orbit");
        assert!(
            output.status.success(),
            "orbit {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn registrations(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|line| line.contains(REGISTRATION_TARGET))
        .count()
}

/// Routine sources: one per registered workspace whose routines were read.
fn sources(routines: &Value) -> Vec<String> {
    let mut sources = routines["routines"]
        .as_array()
        .expect("routine array")
        .iter()
        .filter_map(|routine| routine["source"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

#[test]
fn routine_list_registers_once_across_every_registered_checkout() {
    let host = Host::new();
    let output = host
        .command(host.temp.path())
        .args(["routine", "list", "--format", "json"])
        .output()
        .expect("run orbit routine list");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let routines: Value = serde_json::from_slice(&output.stdout).expect("routine list JSON");
    assert_eq!(
        sources(&routines),
        ["alpha", "beta", "gamma"],
        "routine status opens every registered checkout: {routines}"
    );
    assert_eq!(
        registrations(&stderr),
        1,
        "one registration for the command, none per opened checkout: {stderr}"
    );
}

#[test]
fn dashboard_routine_status_registers_nothing_per_request() {
    let host = Host::new();
    let log = host.temp.path().join("dashboard.stderr");
    let mut server = BoundServer::spawn(
        host.command(host.temp.path())
            .args(["web", "serve", "--port", "0", "--no-open"])
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&log).expect("dashboard log")),
        Announcement::Dashboard,
    )
    .expect("spawn orbit web serve");
    let address = server
        .wait_for_address(STARTUP_DEADLINE)
        .expect("dashboard bound");
    let at_start = registrations(&fs::read_to_string(&log).expect("read dashboard log"));

    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(STARTUP_DEADLINE)
        .build()
        .expect("HTTP client");
    let response = client
        .get(format!("http://{address}/api/routines"))
        .send()
        .expect("GET /api/routines");
    assert!(response.status().is_success(), "{}", response.status());
    let routines: Value = response.json().expect("routine status JSON");
    assert_eq!(
        sources(&routines),
        ["alpha", "beta", "gamma"],
        "routine status opens every registered checkout: {routines}"
    );

    let stderr = fs::read_to_string(&log).expect("read dashboard log");
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(
        at_start, 1,
        "the dashboard joins its generation once at start: {stderr}"
    );
    assert_eq!(
        registrations(&stderr),
        1,
        "a routine status request shares the dashboard's join: {stderr}"
    );
}
