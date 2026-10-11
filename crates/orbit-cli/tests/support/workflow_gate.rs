//! Explicit rendezvous for CLI worker lifecycle tests. The existing
//! `local_shell` action re-executes just the inert waiter below.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

const ADDRESS: &str = "ORBIT_TEST_WORKFLOW_GATE_ADDR";

pub(crate) struct WorkflowGate {
    listener: TcpListener,
    connection: Mutex<Option<TcpStream>>,
}

impl WorkflowGate {
    pub(crate) fn new() -> Self {
        Self {
            listener: TcpListener::bind("127.0.0.1:0").unwrap(),
            connection: Mutex::new(None),
        }
    }

    pub(crate) fn step(&self, id: &str) -> Value {
        let test = std::thread::current()
            .name()
            .expect("gate called by a test")
            .to_owned();
        json!({"id": id, "spec": {"type": "deterministic", "action": "local_shell", "config": {
            "command": std::env::current_exe().unwrap(),
            "args": ["--exact", test, "--nocapture", "--test-threads=1"],
            "env": {ADDRESS: self.listener.local_addr().unwrap().to_string()},
            "timeout_ms": 30000
        }}})
    }

    pub(crate) fn wait_ready(&self) {
        let mut connection = self.connection.lock().unwrap();
        assert!(connection.is_none(), "previous gate must be released");
        let mut fd = libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a single valid descriptor owned by this fixture, for this call.
        assert_eq!(
            unsafe { libc::poll(&mut fd, 1, 30000) },
            1,
            "worker did not enter the gate"
        );
        let (mut stream, _) = self.listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut ready = [0];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(ready, [b'R']);
        *connection = Some(stream);
    }

    pub(crate) fn release(&self) {
        let mut stream = self
            .connection
            .lock()
            .unwrap()
            .take()
            .expect("gate entered");
        stream.write_all(b"G").unwrap();
        let mut done = Vec::new();
        stream.read_to_end(&mut done).unwrap();
        assert!(done.is_empty(), "waiter only signals readiness");
    }
}

impl Drop for WorkflowGate {
    fn drop(&mut self) {
        // Release even on assertion unwinds; a killed worker must not leave
        // its supervised waiter blocked after the fixture is dropped.
        if let Ok(connection) = self.connection.get_mut()
            && let Some(mut stream) = connection.take()
        {
            let _ = stream.write_all(b"G");
        }
    }
}

/// The existing case is just an entrypoint for the supervised waiter.
pub(crate) fn wait_if_requested() -> bool {
    let Ok(address) = std::env::var(ADDRESS) else {
        return false;
    };
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(b"R").unwrap();
    let mut release = [0];
    stream.read_exact(&mut release).unwrap();
    assert_eq!(release, [b'G']);
    true
}
