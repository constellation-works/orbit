//! Public connect regressions. Separate binary because connect installs process
//! signal handlers; each scenario re-executes with a disposable home and PATH.
#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "connect fixtures panic when a disposable home, stream or signal setup fails"
)]

use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use orbit_common::test_env;
use orbit_web::ConnectArgs;

// Implements ssh's forward-ready LocalCommand contract and a session-enabled
// dashboard. Ticket requests are separate ssh invocations, recorded before
// either failing or returning a syntactically valid private-launch URL.
const SSH: &str = r#"#!/usr/bin/env python3
import http.server, os, subprocess, sys
from pathlib import Path

root = Path(os.environ["ORBIT_CONNECT_FIXTURE"])
args = sys.argv[1:]
if "orbit web open --no-open" in args:
    (root / "ticket-request").write_text("\n".join(args))
    if os.environ["ORBIT_CONNECT_MODE"] == "launch-failure":
        print("http://localhost:7878/#orbit-session=" + "A" * 96)
        sys.exit(0)
    sys.exit(1)

port = int(args[args.index("-L") + 1].split(":")[1])
class Dashboard(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"{}")
        if self.path == "/fixture/stop":
            self.wfile.flush()
            os._exit(0)
    def log_message(self, *args):
        pass

server = http.server.HTTPServer(("127.0.0.1", port), Dashboard)
(root / "forward-port").write_text(str(server.server_port))
options = dict(args[i + 1].split("=", 1) for i, arg in enumerate(args) if arg == "-o")
assert options["PermitLocalCommand"] == "yes"
subprocess.run(["/bin/sh", "-c", options["LocalCommand"].replace("%%", "%")], check=True)
server.serve_forever()
"#;

const OPENER: &str = r#"#!/usr/bin/env python3
import os, sys
from pathlib import Path
(Path(os.environ["ORBIT_CONNECT_FIXTURE"]) / "opened-url").write_text(sys.argv[1])
"#;

#[test]
fn browser_session_setup_preserves_connect_tunnel() {
    if let Ok(mode) = std::env::var("ORBIT_CONNECT_CHILD") {
        exercise_connect(mode == "no-open");
        return;
    }
    // Keep default-port selection sequential, including under nextest.
    for mode in ["ticket-failure", "launch-failure", "no-open"] {
        connect_scenario(mode);
        test_env::record_scenario_pass(mode);
    }
}

fn connect_scenario(mode: &str) {
    let name = "browser_session_setup_preserves_connect_tunnel";
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for (name, contents) in [("ssh", SSH), ("xdg-open", OPENER), ("open", OPENER)] {
        let path = bin.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    if mode == "launch-failure" {
        // A refused credential directory must not tear down the SSH tunnel.
        fs::create_dir(root.path().join(".orbit")).unwrap();
        fs::write(root.path().join(".orbit/run"), "not a directory").unwrap();
    }
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("TMPDIR", root.path())
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("ORBIT_CONNECT_CHILD", mode)
        .env("ORBIT_CONNECT_MODE", mode)
        .env("ORBIT_CONNECT_FIXTURE", root.path());
    let output = test_env::run_child_test(&mut command, name, root.path());
    test_env::assert_child_test_passed(name, output.status, output.stdout, output.stderr);
}

fn exercise_connect(no_open: bool) {
    assert!(
        !std::io::stdout().is_terminal(),
        "fixture stdout is captured"
    );
    let root = std::env::var_os("ORBIT_CONNECT_FIXTURE").unwrap();
    let root = Path::new(&root);
    let finished = AtomicBool::new(false);
    let (result, observed) = thread::scope(|scope| {
        let observer = scope.spawn(|| {
            // SIGTERM becomes catchable only once connect reaches its shutdown
            // wait. Read its disposition as a handshake, then end the SSH
            // fixture itself so shutdown need not race signal subscription.
            test_env::wait_until("connect's shutdown wait", || {
                finished.load(Ordering::Acquire) || sigterm_is_caught()
            });
            if finished.load(Ordering::Acquire) {
                return None;
            }
            let port: u16 = fs::read_to_string(root.join("forward-port"))
                .unwrap()
                .parse()
                .unwrap();
            let response = request_tunnel(port, "/healthz");
            assert!(request_tunnel(port, "/fixture/stop").starts_with("HTTP/1.0 200"));
            Some((port, response))
        });
        let result = orbit_web::connect(
            ConnectArgs {
                ssh_host: "fixture-host".into(),
                port: None,
                remote_port: 7878,
                workspace: None,
                global: false,
                no_open,
                no_operator: false,
            },
            None,
        );
        finished.store(true, Ordering::Release);
        (result, observer.join().unwrap())
    });
    assert!(
        result.is_ok(),
        "browser setup must preserve the tunnel: {result:?}"
    );
    let (port, response) = observed.expect("connect must reach its shutdown wait");
    assert!(
        response.starts_with("HTTP/1.0 200"),
        "tunnel must remain usable: {response}"
    );
    if no_open {
        assert!(
            !root.join("ticket-request").exists(),
            "unused ticket fetched over SSH"
        );
        assert!(
            !root.join("opened-url").exists(),
            "--no-open launched a browser"
        );
    } else {
        assert!(
            root.join("ticket-request").exists(),
            "session failure was not exercised"
        );
        let expected_url = format!("http://localhost:{port}");
        test_env::wait_until("plain dashboard browser fallback", || {
            fs::read_to_string(root.join("opened-url")).ok().as_deref() == Some(&expected_url)
        });
        assert_eq!(
            fs::read_to_string(root.join("opened-url")).unwrap(),
            expected_url
        );
    }
}

fn request_tunnel(port: u16, path: &str) -> String {
    let address = (std::net::Ipv4Addr::LOCALHOST, port).into();
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn sigterm_is_caught() -> bool {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: a null new action only reads the process disposition into the
    // valid out pointer. The struct is initialized on success.
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), action.as_mut_ptr()) },
        0
    );
    // SAFETY: sigaction succeeded and initialized action above.
    let handler = unsafe { action.assume_init() }.sa_sigaction;
    handler != libc::SIG_DFL && handler != libc::SIG_IGN
}
