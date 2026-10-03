use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(10);
const CHILD_MARKER: &str = "PROVIDER_INVOCATION_CHILD";

pub const HOSTILE_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GH_TOKEN",
    "DATABASE_URL",
    "ORBIT_OPERATOR",
    "ORBIT_WORKSPACE_CLAIM_TOKEN",
    "ORBIT_UNKNOWN_PRIVILEGE",
];

pub fn scratch() -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp");
    fs::create_dir_all(&root).expect("create fixture scratch root");
    tempfile::Builder::new()
        .prefix("provider-invocation-")
        .tempdir_in(root)
        .expect("isolated fixture directory")
}

/// Re-exec an exact case with disposable home and synthetic ambient secrets.
/// No process-global environment is mutated in the parallel libtest parent.
pub fn isolated(name: &str, run: impl FnOnce()) {
    if std::env::var(CHILD_MARKER).as_deref() == Ok(name) {
        run();
        return;
    }
    let fixture = scratch();
    let log = fixture.path().join("child.log");
    let output = File::create(&log).expect("child log");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("HOME", fixture.path())
        .env("USERPROFILE", fixture.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env(CHILD_MARKER, name)
        .env("ORBIT_RUN_ID", "fixture-run")
        .env("COPILOT_HOME", fixture.path())
        .env("EXPLICIT_PROVIDER_SETTING", "opted-in")
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .stdout(output.try_clone().expect("child stdout"))
        .stderr(output);
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    // Stamp synthetic context only after ambient authority has been cleared.
    command.env("ORBIT_RUN_ID", "fixture-run");
    for key in HOSTILE_ENV {
        command.env(key, format!("synthetic-{key}"));
    }
    let mut child = ChildGuard(command.spawn().expect("isolated test child"));
    let status = child.wait(Duration::from_secs(30));
    let output = fs::read_to_string(log).expect("read child output");
    assert!(status.success(), "isolated {name} failed:\n{output}");
    assert!(
        output.contains("1 passed"),
        "isolated case must actually run: {output}"
    );
}

pub struct ChildGuard(pub Child);

impl ChildGuard {
    pub fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().expect("poll child") {
                return status;
            }
            assert!(Instant::now() < deadline, "child exceeded {timeout:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub struct RecordedRequest {
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Real HTTP wire fixture. Accept, socket I/O and receiving records are bounded;
/// a missing request cannot leave a blocking accept/join behind.
pub struct Server {
    pub base_url: String,
    requests: Receiver<RecordedRequest>,
    finished: Receiver<()>,
}

impl Server {
    pub fn new(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fake HTTP server");
        let base_url = format!("http://{}", listener.local_addr().expect("server address"));
        listener.set_nonblocking(true).expect("nonblocking accept");
        let (tx, requests) = mpsc::sync_channel(responses.len());
        let (done_tx, finished) = mpsc::sync_channel(1);
        thread::spawn(move || {
            for response in responses {
                let deadline = Instant::now() + WAIT;
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "HTTP accept timed out");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(err) => panic!("HTTP accept failed: {err}"),
                    }
                };
                stream.set_read_timeout(Some(WAIT)).expect("read timeout");
                stream.set_write_timeout(Some(WAIT)).expect("write timeout");
                let request = read_request(&mut stream);
                let bytes = serde_json::to_vec(&response).expect("response JSON");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .expect("response headers");
                stream.write_all(&bytes).expect("response body");
                if tx.send(request).is_err() {
                    return;
                }
            }
            let _ = done_tx.send(());
        });
        Self {
            base_url,
            requests,
            finished,
        }
    }

    pub fn request(&self) -> RecordedRequest {
        self.requests
            .recv_timeout(WAIT)
            .expect("recorded HTTP request")
    }

    pub fn finish(&self) {
        self.finished
            .recv_timeout(WAIT)
            .expect("HTTP fixture finished");
    }
}

fn read_request(stream: &mut TcpStream) -> RecordedRequest {
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).expect("read HTTP request");
        assert!(n > 0, "request ended before headers");
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() <= 1024 * 1024, "fixture request too large");
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec()).expect("HTTP headers UTF-8");
    let mut lines = head.lines();
    let target = lines.next().expect("request line").to_string();
    let headers: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .expect("request content length")
        .1
        .parse()
        .expect("numeric content length");
    assert!(length <= 1024 * 1024, "fixture body too large");
    while bytes.len() < header_end + length {
        let n = stream.read(&mut buf).expect("read request body");
        assert!(n > 0, "request ended before body");
        bytes.extend_from_slice(&buf[..n]);
    }
    RecordedRequest {
        target,
        headers,
        body: serde_json::from_slice(&bytes[header_end..header_end + length])
            .expect("request JSON"),
    }
}
