//! Own substitute landing workers until cancellation/exit and supervisor reap.

use super::*;

// The listener and streams belong to this fixture, including during unwind.
// No worker can exit before its owning fixture releases it, and the TempDir
// outlives every real supervisor reap. This also covers concurrent retry runs
// created through the public boundary, without serializing those requests.
pub(super) struct LandingWorkers {
    listener: std::net::TcpListener,
    owned: Mutex<BTreeMap<String, Vec<(u32, std::net::TcpStream)>>>,
    persisted_only: Mutex<BTreeSet<String>>,
}

impl LandingWorkers {
    pub(super) fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let entry = "distributed_drain::landing_repair::worker::substitute_landing_worker";
        orbit_common::test_env::assert_child_test_exists(entry);
        orbit_core::test_support::install_substitute_pipeline_worker([
            "env".to_string(),
            format!(
                "ORBIT_LANDING_FIXTURE_CONTROL={}",
                listener.local_addr().unwrap()
            ),
            format!(
                "ORBIT_LANDING_FIXTURE_RUN={}",
                orbit_core::test_support::RUN_ID_PLACEHOLDER
            ),
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--exact".into(),
            entry.into(),
            "--ignored".into(),
            "--nocapture".into(),
        ]);
        Self {
            listener,
            owned: Mutex::new(BTreeMap::new()),
            persisted_only: Mutex::new(BTreeSet::new()),
        }
    }

    pub(super) fn persisted_only(&self, run: &str) {
        self.persisted_only.lock().unwrap().insert(run.to_string());
    }

    pub(super) fn is_persisted_only(&self, run: &str) -> bool {
        self.persisted_only.lock().unwrap().contains(run)
    }

    pub(super) fn pid(&self, run: &str) -> Option<u32> {
        self.owned
            .lock()
            .unwrap()
            .get(run)
            .map(|workers| workers[0].0)
    }

    pub(super) fn own(&self, run: &str) -> std::io::Result<u32> {
        self.own_count(run, 1)
    }

    pub(super) fn own_count(&self, run: &str, count: usize) -> std::io::Result<u32> {
        use std::io::BufRead;
        let mut owned = self.owned.lock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while owned.get(run).map_or(0, Vec::len) < count {
            let (stream, _) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "landing worker ownership handshake",
                        ));
                    }
                    std::thread::park_timeout(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(error),
            };
            // Darwin inherits O_NONBLOCK from this fixture's listener. Accept
            // proves a connection is queued, not that the substitute has
            // written its identity. SO_RCVTIMEO does not make a nonblocking
            // read wait, so own_count returned EAGAIN (errno 35) under
            // parallel load before the second worker's identity line arrived
            // [F2026-10-457].
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut reader = std::io::BufReader::new(stream);
            let mut identity = String::new();
            reader.read_line(&mut identity)?;
            let (id, pid) = identity
                .trim()
                .split_once(' ')
                .ok_or_else(|| std::io::Error::other("landing worker identity"))?;
            let pid = pid.parse::<u32>().map_err(std::io::Error::other)?;
            #[cfg(unix)]
            if !orbit_common::process::ancestry::process_start_and_parent(pid)
                .is_some_and(|(_, parent)| parent == std::process::id())
            {
                return Err(std::io::Error::other(
                    "landing substitute is not this fixture's child",
                ));
            }
            let workers = owned.entry(id.to_string()).or_default();
            if workers.iter().any(|(owned_pid, _)| *owned_pid == pid) {
                return Err(std::io::Error::other("duplicate landing worker identity"));
            }
            workers.push((pid, reader.into_inner()));
            let _ = writeln!(
                std::io::stderr(),
                "owned landing worker: run={id} pid={pid}"
            );
        }
        Ok(owned[run][0].0)
    }

    pub(super) fn release(&self, run: &str) -> std::io::Result<()> {
        use std::io::Write;
        let mut owned = self.owned.lock().unwrap();
        let workers = owned
            .get_mut(run)
            .ok_or_else(|| std::io::Error::other("unowned landing worker"))?;
        for (pid, stream) in workers {
            if landing_worker_reaped(*pid) {
                continue;
            }
            stream.write_all(&[1])?;
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !landing_worker_reaped(*pid) {
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "landing supervisor reap",
                    ));
                }
                std::thread::park_timeout(Duration::from_millis(10));
            }
            let _ = writeln!(
                std::io::stderr(),
                "landing worker reaped: run={run} pid={pid}"
            );
        }
        Ok(())
    }
}

fn landing_worker_reaped(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal zero observes zombies too: ESRCH proves the supervisor reaped.
        (unsafe { libc::kill(pid as libc::pid_t, 0) }) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        !orbit_common::process::identity::process_is_alive(pid)
    }
}

#[test]
#[ignore = "entry point for owned landing substitute workers"]
fn substitute_landing_worker() {
    use std::io::{Read, Write};
    let address = std::env::var("ORBIT_LANDING_FIXTURE_CONTROL").unwrap();
    let run = std::env::var("ORBIT_LANDING_FIXTURE_RUN").unwrap();
    let mut control =
        std::net::TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_secs(5))
            .unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    writeln!(control, "{run} {}", std::process::id()).unwrap();
    let mut release = [0];
    control.read_exact(&mut release).unwrap();
    assert_eq!(release, [1]);
}
